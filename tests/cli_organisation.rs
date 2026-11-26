//! Portable organisation commands, including real-process edit and crash seams.
#![cfg(feature = "storage-test-hooks")]
#[path = "support/local_days.rs"]
mod local_days;
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
            .env("TZ", "UTC0")
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
        vec!["ls", "--tag", "a b"],
        vec!["ls", "--contests", "--status", "new"],
        vec!["status", "--solved", "2026-02-29"],
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
    // Search terms: none, 17, one of 257 bytes and (Unix) one not UTF-8.
    #[cfg(unix)]
    let non_utf8 = {
        use std::os::unix::ffi::OsStringExt;
        Some(std::ffi::OsString::from_vec(vec![b'a', 0xff]))
    };
    #[cfg(not(unix))]
    let non_utf8: Option<std::ffi::OsString> = None;
    let searches = [
        vec![std::ffi::OsString::from("search")],
        std::iter::once("search".into())
            .chain((0..17).map(|index| format!("t{index}").into()))
            .collect::<Vec<std::ffi::OsString>>(),
        vec!["search".into(), "x".repeat(257).into()],
    ]
    .into_iter()
    .chain(non_utf8.map(|term| vec!["search".into(), term]));
    for args in searches {
        let output = Command::new(env!("CARGO_BIN_EXE_cpf"))
            .current_dir(&outside.0)
            .args(&args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
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

/// The shared twelve-problem organisation fixture. Every reader test uses this
/// constructor so membership, ordering and invalid fields have one definition.
fn fixture_f() -> Workspace {
    let workspace = Workspace::new();
    fs::remove_dir_all(workspace.path("problems/p")).unwrap();
    let rows = [
        ("a", "local:a", "", "tags = ['DP']", "2026-11-24"),
        (
            "atcoder-abc400_a",
            "atcoder:abc400_a",
            "solved",
            "tags = ['dp', 'graphs']",
            "2026-11-24T10:00:00Z",
        ),
        ("b", "local:b", "new", "tags = ['graphs']", NOW),
        ("c", "local:c", "in-progress", "tags = ['dp']", "invalid"),
        ("d", "local:d", "solved", "tags = ['DP', 'graphs']", ""),
        ("e", "local:e", "skipped", "", ""),
        ("wa", "local:wa", "wa", "tags = ['dp']", ""),
        ("g", "local:g", "5", "tags = ['dp']", ""),
        ("h", "local:h", "solved", "tags = 'dp'", ""),
        ("i", "local:i", "new", "", ""),
        ("j", "local:j", "skipped", "tags = ['dp']", ""),
        ("k", "local:k", "solved", "", ""),
    ];
    for (name, identity, status, tags, date) in rows {
        let mut record = format!("schema = 1\n[problem]\nid = '{identity}'\n{tags}\n");
        if !status.is_empty() {
            record.push_str(&format!("status = '{status}'\n"));
        }
        if !date.is_empty() {
            for field in ["created_at", "updated_at", "solved_at", "archived_at"] {
                record.push_str(&format!("{field} = '{date}'\n"));
            }
        }
        workspace.write(&format!("problems/{name}/problem.toml"), record);
    }
    for (name, ids) in [
        ("c1", vec!["local:d", "atcoder:abc400_a", "local:a"]),
        ("c2", vec!["local:b", "atcoder:abc400_a", "local:wa"]),
    ] {
        let mut record = "schema = 1\ntags = ['rated']\n".to_owned();
        for id in ids {
            record.push_str(&format!("[[problem]]\nid = '{id}'\n"));
        }
        workspace.write(&format!("contests/{name}/contest.toml"), record);
    }
    workspace
}

fn document(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap()
}
fn problem_ids(output: &Output, json: bool) -> Vec<String> {
    if json {
        document(output)["problems"]
            .as_array()
            .unwrap()
            .iter()
            .map(|problem| problem["problem"].as_str().unwrap().to_owned())
            .collect()
    } else {
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| line.starts_with("local:") || line.starts_with("atcoder:"))
            .map(|line| line.split('\t').next().unwrap().to_owned())
            .collect()
    }
}

#[test]
fn shared_filters_keep_candidate_order_and_validated_values() {
    let workspace = fixture_f();
    let mut table = vec![
        (
            vec!["--status", "new"],
            vec!["local:a", "local:b", "local:i"],
        ),
        (vec!["--status", "in-progress"], vec!["local:c"]),
        (
            vec!["--status", "solved"],
            vec!["atcoder:abc400_a", "local:d", "local:h", "local:k"],
        ),
        (vec!["--status", "skipped"], vec!["local:e", "local:j"]),
        (vec!["--status", "wa"], vec!["local:wa"]),
        (
            vec!["--status", "solved", "--status", "new"],
            vec![
                "local:a",
                "atcoder:abc400_a",
                "local:b",
                "local:d",
                "local:h",
                "local:i",
                "local:k",
            ],
        ),
        (
            vec!["--tag", "dp"],
            vec![
                "local:a",
                "atcoder:abc400_a",
                "local:c",
                "local:d",
                "local:g",
                "local:j",
                "local:wa",
            ],
        ),
        (
            vec!["--tag", "DP"],
            vec![
                "local:a",
                "atcoder:abc400_a",
                "local:c",
                "local:d",
                "local:g",
                "local:j",
                "local:wa",
            ],
        ),
        (
            vec!["--tag", "dp", "--tag", "graphs"],
            vec!["atcoder:abc400_a", "local:d"],
        ),
        (vec!["--tag", "rated"], vec![]),
        (vec!["--site", "atcoder"], vec!["atcoder:abc400_a"]),
        (
            vec!["--site", "local"],
            vec![
                "local:a", "local:b", "local:c", "local:d", "local:e", "local:g", "local:h",
                "local:i", "local:j", "local:k", "local:wa",
            ],
        ),
        (
            vec!["--contest", "c1"],
            vec!["local:a", "atcoder:abc400_a", "local:d"],
        ),
        (
            vec!["--contest", "c1", "--contest", "c2"],
            vec![
                "local:a",
                "atcoder:abc400_a",
                "local:b",
                "local:d",
                "local:wa",
            ],
        ),
        (
            vec!["--status", "solved", "--tag", "dp"],
            vec!["atcoder:abc400_a", "local:d"],
        ),
        (
            vec!["--contest", "c1", "--site", "local", "--tag", "dp"],
            vec!["local:a", "local:d"],
        ),
        (
            vec!["--site", "local", "--site", "atcoder", "--status", "wa"],
            vec!["local:wa"],
        ),
        (vec!["--status", "removed-custom-value"], vec![]),
    ];
    for flag in ["--created", "--updated", "--solved", "--archived-in"] {
        for range in [
            "2026-11-24",
            "2026-11-24..",
            "..2026-11-24",
            "2026-11-20..2026-11-24",
        ] {
            table.push((
                vec![flag, range],
                vec!["local:a", "atcoder:abc400_a", "local:b"],
            ));
        }
        for range in ["..2026-11-23", "2026-11-25..", "2026-11-20..2026-11-23"] {
            table.push((vec![flag, range], vec![]));
        }
        table.push((
            vec![flag, "2026-11-24", "--tag", "dp"],
            vec!["local:a", "atcoder:abc400_a"],
        ));
        table.push((
            vec![flag, "2026-11-24", "--status", "solved"],
            vec!["atcoder:abc400_a"],
        ));
    }
    table.push((
        vec![
            "--created",
            "2026-11-24",
            "--updated",
            "..2026-11-24",
            "--solved",
            "2026-11-24..",
            "--archived-in",
            "2026-11-20..2026-11-24",
            "--contest",
            "c1",
            "--site",
            "local",
        ],
        vec!["local:a"],
    ));
    for (filters, expected) in table {
        for prefix in [vec!["ls"], vec!["status", "--all"], vec!["status"]] {
            for json in [false, true] {
                let mut args = prefix.clone();
                args.extend_from_slice(&filters);
                if json {
                    args.push("--json");
                }
                let output = workspace.unchanged(&args, 0);
                assert_eq!(problem_ids(&output, json), expected, "{args:?}");
                if prefix[0] == "ls" && json {
                    let doc = document(&output);
                    assert!(
                        doc["invalid"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|source| source["field"] == "status")
                    );
                    for field in ["created_at", "updated_at", "solved_at", "archived_at"] {
                        assert!(
                            doc["invalid"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .any(|source| source["field"] == field)
                        );
                    }
                }
            }
        }
    }
    for json in [false, true] {
        let mut args = vec!["status", "contest:c1", "--status", "solved"];
        if json {
            args.push("--json");
        }
        assert_eq!(
            problem_ids(&workspace.unchanged(&args, 0), json),
            ["local:d", "atcoder:abc400_a"]
        );
    }
    for prefix in [vec!["ls"], vec!["status", "--all"]] {
        let mut args = prefix;
        args.extend(["--contest", "nope"]);
        workspace.unchanged(&args, 8);
    }
    workspace.unchanged(&["ls", "--status", "5"], 2);
    workspace.unchanged(&["status", "--status", "5"], 2);
}

#[test]
fn invalid_entries_and_fields_survive_filters() {
    let workspace = fixture_f();
    workspace.write("problems/broken/problem.toml", "not [ toml");
    workspace.write("problems/f", "regular file");
    workspace.write("problems/future/problem.toml", "schema = 2\n");
    workspace.write(
        "problems/mismatch/problem.toml",
        "schema = 1\n[problem]\nid = 'local:other'\n",
    );
    workspace.write("problems/missing/placeholder", "");
    fs::create_dir_all(workspace.path("problems/unreadable/problem.toml")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("a", workspace.path("problems/l")).unwrap();
    #[cfg(windows)]
    {
        let status = Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(workspace.path("problems/l"))
            .arg(workspace.path("problems/a"))
            .status()
            .unwrap();
        assert!(status.success());
    }
    let output = workspace.unchanged(&["ls", "--json"], 0);
    let doc = document(&output);
    assert_eq!(doc["problems"].as_array().unwrap().len(), 12);
    let sources = doc["invalid"].as_array().unwrap();
    for (path, kind) in [
        ("problems/broken/problem.toml", "metadata_malformed"),
        ("problems/f", "not_a_directory"),
        ("problems/l", "link"),
        ("problems/future/problem.toml", "metadata_too_new"),
        ("problems/mismatch/problem.toml", "identity_mismatch"),
        ("problems/missing/problem.toml", "metadata_missing"),
        ("problems/unreadable/problem.toml", "metadata_unreadable"),
    ] {
        assert!(
            sources
                .iter()
                .any(|source| source["path"]["utf8"] == path && source["kind"] == kind),
            "{path}: {sources:?}"
        );
        let human = workspace.unchanged(&["ls", "--tag", "rated"], 0);
        assert!(String::from_utf8_lossy(&human.stdout).contains(kind));
    }
    // The filter keeps the unfiltered `--all` failure: same status, same entry.
    let filtered = workspace.unchanged(&["status", "--all", "--tag", "dp", "--json"], 8);
    let unfiltered = workspace.unchanged(&["status", "--all", "--json"], 8);
    let message = document(&filtered)["error"]["message"].clone();
    assert!(message.as_str().unwrap().contains("broken"), "{message}");
    assert_eq!(message, document(&unfiltered)["error"]["message"]);
    let invalid = doc["problems"]
        .as_array()
        .unwrap()
        .iter()
        .find(|problem| problem["problem"] == "local:g")
        .unwrap();
    assert!(invalid["user_status"].is_null() && invalid["effective_status"].is_null());
    let invalid = doc["problems"]
        .as_array()
        .unwrap()
        .iter()
        .find(|problem| problem["problem"] == "local:h")
        .unwrap();
    assert_eq!(invalid["tags"], serde_json::json!([]));
    assert_eq!(invalid["findings"][0]["field"], "tags");
}

#[test]
fn contest_counts_are_derived_and_contest_filters_use_contest_fields() {
    let workspace = fixture_f();
    // Five members: two solved, one absent, one invalid and one unresolved.
    workspace.write("contests/c/contest.toml", "schema = 1\ntags = ['Rated']\n[[problem]]\nid = 'local:d'\n[[problem]]\nid = 'local:k'\n[[problem]]\nid = 'local:a'\n[[problem]]\nid = 'local:g'\n[[problem]]\nid = 'local:missing'\n");
    workspace.write("contests/atcoder-abc400/contest.toml", "schema = 1\ntags = ['rated']\n[contest]\nid = 'atcoder:abc400'\n[[problem]]\nid = 'atcoder:abc400_a'\n");
    workspace.write("contests/broken/contest.toml", "not [ toml");
    workspace.write("contests/f", "regular file");
    fs::remove_dir_all(workspace.path("contests/c1")).unwrap();
    fs::remove_dir_all(workspace.path("contests/c2")).unwrap();
    let before = fs::read(workspace.path("contests/c/contest.toml")).unwrap();
    let output = workspace.unchanged(&["ls", "--contests", "--json"], 0);
    let doc = document(&output);
    assert_eq!(doc["contests"][0]["contest"], "atcoder-abc400");
    let c = &doc["contests"][1];
    assert_eq!(c["contest"], "c");
    assert_eq!(c["tags"], serde_json::json!(["Rated"]));
    assert_eq!(c["members"], 5);
    assert_eq!(
        c["counts"],
        serde_json::json!([{"status":"new","count":1},{"status":"solved","count":2}])
    );
    assert_eq!(c["invalid"], 1);
    assert_eq!(c["unresolved"], 1);
    assert!(
        doc["invalid"]
            .as_array()
            .unwrap()
            .iter()
            .any(|source| source["kind"] == "contest_malformed")
    );
    assert!(
        doc["invalid"]
            .as_array()
            .unwrap()
            .iter()
            .any(|source| source["kind"] == "not_a_directory")
    );
    for (filters, expected) in [
        (vec!["--site", "atcoder"], vec!["atcoder-abc400"]),
        (vec!["--site", "local"], vec!["c"]),
        (vec!["--tag", "rated"], vec!["atcoder-abc400", "c"]),
        (vec!["--tag", "dp"], vec![]),
    ] {
        let mut args = vec!["ls", "--contests", "--json"];
        args.extend(filters);
        let output = workspace.unchanged(&args, 0);
        let doc = document(&output);
        let actual = doc["contests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|contest| contest["contest"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }
    let human = workspace.unchanged(&["ls", "--contests"], 0);
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(text.contains("new 1, solved 2, invalid 1, unresolved 1"));
    assert!(text.contains("Rated\t5\t"));
    workspace.run(&["mark", "a", "solved"], 0);
    let doc = document(&workspace.unchanged(&["ls", "--contests", "--json"], 0));
    assert_eq!(
        doc["contests"][1]["counts"],
        serde_json::json!([{"status":"solved","count":3}])
    );
    assert_eq!(
        fs::read(workspace.path("contests/c/contest.toml")).unwrap(),
        before
    );
    workspace.unchanged(&["ls", "--contest", "broken", "--json"], 8);
}

#[test]
fn readers_create_no_lock_and_ignore_a_held_lock() {
    let workspace = fixture_f();
    fs::remove_file(workspace.path(".cpforge/lock")).unwrap();
    for args in [
        vec!["ls"],
        vec!["search", "dp"],
        vec!["ls", "--contests"],
        vec!["status", "--tag", "dp"],
        vec!["notes", "a", "--path"],
    ] {
        workspace.unchanged(&args, 0);
        assert!(!workspace.path(".cpforge/lock").exists());
    }
    let lock = workspace.0.lock();
    let held = lock.acquire("reader exclusion check").unwrap();
    // A Windows lock handle is exclusive; snapshots must skip it while held.
    for args in [
        vec!["ls", "--json"],
        vec!["search", "dp", "--json"],
        vec!["ls", "--contests", "--json"],
        vec!["status", "--tag", "dp", "--json"],
    ] {
        workspace.run(&args, 0);
    }
    drop(held);
}

#[test]
fn listing_workspace_errors_and_closed_json_stdout() {
    let outside = Directory::new();
    for json in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cpf"));
        command.current_dir(&outside.0).arg("ls");
        if json {
            command.arg("--json");
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(8));
        if json {
            assert_eq!(document(&output)["document"], "error");
        }
    }
    let workspace = fixture_f();
    let mut child = workspace
        .command(&["ls", "--json"])
        .stdout(closed_pipe())
        .spawn()
        .unwrap();
    assert_eq!(child.wait().unwrap().code(), Some(11));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for (path, args) in [
            ("problems", vec!["ls", "--json"]),
            ("contests", vec!["ls", "--contests", "--json"]),
        ] {
            let path = workspace.path(path);
            let permissions = fs::metadata(&path).unwrap().permissions();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
            // Root can enumerate a mode-000 directory; exercise when enforced.
            let denied = fs::read_dir(&path).is_err();
            let output = workspace.run(&args, if denied { 8 } else { 0 });
            fs::set_permissions(&path, permissions).unwrap();
            if denied {
                assert_eq!(document(&output)["document"], "error");
            }
        }
    }
}

// Own only the write end of an anonymous pipe. No reader exists by the time
// cpf starts, so this cannot race a fast successful JSON publication.
fn closed_pipe() -> Stdio {
    #[cfg(unix)]
    {
        use std::os::fd::FromRawFd;
        let mut handles = [-1; 2];
        // SAFETY: handles is writable storage for two file descriptors.
        assert_eq!(unsafe { libc::pipe(handles.as_mut_ptr()) }, 0);
        // SAFETY: pipe returned two distinct descriptors owned by this helper.
        unsafe {
            drop(File::from_raw_fd(handles[0]));
            Stdio::from(File::from_raw_fd(handles[1]))
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::FromRawHandle;
        use windows_sys::Win32::System::Pipes::CreatePipe;
        let mut reader = std::ptr::null_mut();
        let mut writer = std::ptr::null_mut();
        // SAFETY: both out parameters are writable; default pipe attributes.
        assert_ne!(
            unsafe { CreatePipe(&mut reader, &mut writer, std::ptr::null(), 0) },
            0
        );
        // SAFETY: CreatePipe returned two distinct handles owned by this helper.
        unsafe {
            drop(File::from_raw_handle(reader));
            Stdio::from(File::from_raw_handle(writer))
        }
    }
}

fn assert_local_day(workspace: &Workspace, zone: &std::ffi::OsStr, stored: &str, day: &str) {
    workspace.write(
        "problems/p/problem.toml",
        format!("{BASE}solved_at = '{stored}'\n"),
    );
    let before = snapshot(&workspace.0.0);
    for command in ["ls", "status"] {
        for json in [false, true] {
            let mut args = vec![command, "--solved", day];
            if json {
                args.push("--json");
            }
            let output = workspace.command(&args).env("TZ", zone).output().unwrap();
            assert!(output.status.success(), "{zone:?} {args:?}: {output:?}");
            assert_eq!(
                problem_ids(&output, json),
                ["local:p"],
                "{stored}: {args:?}"
            );
            assert!(output.stderr.is_empty(), "{output:?}");
            if command == "ls" && json {
                let expected = if stored.len() == 10 {
                    format!("{stored}T00:00:00.000Z")
                } else {
                    stored.to_owned()
                };
                assert_eq!(document(&output)["problems"][0]["solved_at"], expected);
            }
        }
        // Neither neighbouring local day also contains this value.
        let date: jiff::civil::Date = day.parse().unwrap();
        for other in [date.yesterday(), date.tomorrow()] {
            let Ok(other) = other else { continue };
            let other = other.to_string();
            let output = workspace
                .command(&[command, "--solved", other.as_str(), "--json"])
                .env("TZ", zone)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            assert!(
                problem_ids(&output, true).is_empty(),
                "{zone:?} {stored} in {other}: {output:?}"
            );
        }
    }
    assert_eq!(snapshot(&workspace.0.0), before);
}

#[test]
fn tz_posix_boundaries_gaps_folds_dates_and_domain_edges_end_to_end() {
    let workspace = Workspace::new();
    for &(rule, stored, day) in local_days::POSIX_CASES {
        assert_local_day(&workspace, rule.as_ref(), stored, day);
    }
    for (range, expected) in [("..1970-01-01", vec!["local:p"]), ("1970-01-01", vec![])] {
        for command in ["ls", "status"] {
            let output = workspace
                .command(&[command, "--solved", range, "--json"])
                .env("TZ", "<-12>12")
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            assert_eq!(problem_ids(&output, true), expected);
        }
    }
}

#[test]
fn tzif_paths_use_historical_rules_and_the_future_footer_end_to_end() {
    let workspace = Workspace::new();
    for &(name, stored, day) in local_days::TZIF_CASES {
        let mut path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("timezones");
        for component in name.split('/') {
            path.push(component);
        }
        assert_local_day(&workspace, path.as_os_str(), stored, day);
    }
    #[cfg(unix)]
    {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("timezones")
            .join("Europe")
            .join("Berlin");
        let prefixed = format!(":{}", path.display());
        for (stored, day) in [
            ("2026-11-24T23:30:00.000Z", "2026-11-25"),
            ("2026-07-01T22:30:00.000Z", "2026-07-02"),
            ("2026-07-01T21:59:59.999Z", "2026-07-01"),
        ] {
            assert_local_day(&workspace, path.as_os_str(), stored, day);
            assert_local_day(&workspace, prefixed.as_ref(), stored, day);
        }
    }
}

#[test]
fn invalid_tz_warns_once_and_falls_back_to_utc_only_with_date_filters() {
    let workspace = Workspace::new();
    workspace.write("problems/p/problem.toml", format!("{BASE}created_at = '2026-11-24T23:30:00.000Z'\nsolved_at = '2026-11-24T23:30:00.000Z'\n"));
    let before = snapshot(&workspace.0.0);
    for command in ["ls", "status"] {
        for json in [false, true] {
            let mut args = vec![command, "--created", "2026-11-24", "--solved", "2026-11-24"];
            if json {
                args.push("--json");
            }
            let output = workspace
                .command(&args)
                .env("TZ", "Nowhere/Invalid")
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            assert_eq!(problem_ids(&output, json), ["local:p"]);
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert_eq!(stderr.lines().count(), 1, "{stderr}");
            assert!(
                stderr.contains("warning")
                    && stderr.contains("Nowhere/Invalid")
                    && stderr.contains("UTC"),
                "{stderr}"
            );
        }
        let output = workspace
            .command(&[command, "--tag", "dp"])
            .env("TZ", "Nowhere/Invalid")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
    }
    let output = workspace
        .command(&["ls", "--contests"])
        .env("TZ", "Nowhere/Invalid")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert_eq!(snapshot(&workspace.0.0), before);
}

#[test]
fn invalid_date_ranges_fail_before_discovery_or_workspace_lookup() {
    let outside = Directory::new();
    for range in [
        "2026-11-25..2026-11-24",
        "..",
        "",
        "2026-02-29",
        "2026-11-24T10:00Z",
    ] {
        for command in ["ls", "status"] {
            for flag in ["--created", "--updated", "--solved", "--archived-in"] {
                let output = Command::new(env!("CARGO_BIN_EXE_cpf"))
                    .current_dir(&outside.0)
                    .args([command, flag, range])
                    .env("TZ", "Nowhere/Invalid")
                    .output()
                    .unwrap();
                assert_eq!(output.status.code(), Some(2), "{output:?}");
                assert!(output.stdout.is_empty());
                assert!(
                    !String::from_utf8_lossy(&output.stderr).contains("warning"),
                    "{output:?}"
                );
                assert_eq!(fs::read_dir(&outside.0).unwrap().count(), 0);
            }
        }
    }
}

#[cfg(windows)]
struct HostTimeZone(String);

#[cfg(windows)]
impl HostTimeZone {
    fn save() -> Self {
        use windows_sys::Win32::System::Time::{
            DYNAMIC_TIME_ZONE_INFORMATION, GetDynamicTimeZoneInformation, TIME_ZONE_ID_INVALID,
        };
        let mut info = DYNAMIC_TIME_ZONE_INFORMATION::default();
        // SAFETY: info is valid writable storage for this system call.
        assert_ne!(
            unsafe { GetDynamicTimeZoneInformation(&mut info) },
            TIME_ZONE_ID_INVALID
        );
        let output = Command::new("tzutil").arg("/g").output().unwrap();
        assert!(output.status.success(), "{output:?}");
        let mut name = String::from_utf8(output.stdout).unwrap().trim().to_owned();
        if info.DynamicDaylightTimeDisabled && !name.ends_with("_dstoff") {
            name.push_str("_dstoff");
        }
        Self(name)
    }
}

#[cfg(windows)]
impl Drop for HostTimeZone {
    fn drop(&mut self) {
        // tzutil enables the required time-zone privilege in its own process.
        // Restore the host zone and disabled-DST setting even during unwinding.
        let restored = Command::new("tzutil")
            .args(["/s", self.0.as_str()])
            .status();
        if !std::thread::panicking() {
            assert!(
                restored.is_ok_and(|status| status.success()),
                "restore host time zone"
            );
        }
    }
}

#[cfg(windows)]
#[test]
fn windows_system_zone_and_dst_disabled_match_the_contract() {
    let _restore = HostTimeZone::save();
    let workspace = Workspace::new();
    let berlin = jiff::tz::TimeZone::tzif(
        "Europe/Berlin",
        include_bytes!("fixtures/timezones/Europe/Berlin"),
    )
    .unwrap();
    for (name, disabled) in [
        ("W. Europe Standard Time", false),
        ("W. Europe Standard Time_dstoff", true),
    ] {
        let output = Command::new("tzutil").args(["/s", name]).output().unwrap();
        assert!(output.status.success(), "{name}: {output:?}");
        for &(rule, stored, expected) in local_days::POSIX_CASES {
            if rule != local_days::CET || stored.len() == 10 {
                continue;
            }
            let day = if disabled && stored == "2026-07-01T22:30:00.000Z" {
                "2026-07-01"
            } else {
                expected
            };
            if !disabled {
                let instant: jiff::Timestamp = stored.parse().unwrap();
                assert_eq!(berlin.to_datetime(instant).date().to_string(), day);
            }
            workspace.write(
                "problems/p/problem.toml",
                format!("{BASE}solved_at = '{stored}'\n"),
            );
            for command in ["ls", "status"] {
                for empty in [false, true] {
                    let mut child = workspace.command(&[command, "--solved", day, "--json"]);
                    if empty {
                        child.env("TZ", "");
                    } else {
                        child.env_remove("TZ");
                    }
                    let output = child.output().unwrap();
                    assert!(output.status.success(), "{name}: {output:?}");
                    assert_eq!(problem_ids(&output, true), ["local:p"]);
                    assert!(output.stderr.is_empty(), "{output:?}");
                }
            }
        }
    }
}

#[test]
fn date_flags_use_their_own_fields_and_combine_with_and() {
    let workspace = Workspace::new();
    workspace.write("problems/p/problem.toml", format!("{BASE}created_at = '2026-11-24T23:30:00Z'\nupdated_at = '2026-11-24'\nsolved_at = '2026-11-25T23:30:00.000Z'\narchived_at = '2026-11-26T23:30:00.000Z'\n"));
    for (filters, expected) in [
        (vec!["--created", "2026-11-25"], vec!["local:p"]),
        (vec!["--updated", "2026-11-24"], vec!["local:p"]),
        (vec!["--solved", "2026-11-26"], vec!["local:p"]),
        (vec!["--archived-in", "2026-11-27"], vec!["local:p"]),
        (vec!["--created", "2026-11-24"], vec![]),
        (vec!["--updated", "2026-11-25"], vec![]),
        (vec!["--solved", "2026-11-25"], vec![]),
        (vec!["--archived-in", "2026-11-26"], vec![]),
        (
            vec![
                "--created",
                "2026-11-25",
                "--updated",
                "2026-11-24",
                "--solved",
                "2026-11-26",
                "--archived-in",
                "2026-11-27",
            ],
            vec!["local:p"],
        ),
        (
            vec![
                "--created",
                "2026-11-25",
                "--updated",
                "2026-11-25",
                "--solved",
                "2026-11-26",
                "--archived-in",
                "2026-11-27",
            ],
            vec![],
        ),
    ] {
        let before = snapshot(&workspace.0.0);
        for prefix in [vec!["ls"], vec!["status", "--all"], vec!["status"]] {
            for json in [false, true] {
                let mut args = prefix.clone();
                args.extend_from_slice(&filters);
                if json {
                    args.push("--json");
                }
                let output = workspace
                    .command(&args)
                    .env("TZ", local_days::CET)
                    .output()
                    .unwrap();
                assert!(output.status.success(), "{output:?}");
                assert_eq!(problem_ids(&output, json), expected, "{args:?}");
            }
        }
        assert_eq!(snapshot(&workspace.0.0), before);
    }
}

fn search_doc(workspace: &Workspace, terms: &[&str]) -> serde_json::Value {
    let mut args = vec!["search"];
    args.extend_from_slice(terms);
    args.push("--json");
    document(&workspace.unchanged(&args, 0))
}
fn hit_ids(value: &serde_json::Value) -> Vec<&str> {
    value["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| hit["problem"].as_str().unwrap())
        .collect()
}
fn reasons(value: &serde_json::Value) -> Vec<&str> {
    value["incomplete_reasons"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect()
}

#[test]
fn search_hits_unicode_fields_filters_and_no_matches() {
    let w = fixture_f();
    w.write("problems/a/notes.md", "cafe\u{301}\r\nonlynotes\n");
    w.write("problems/b/notes.md", "café\n");
    w.write("problems/d/notes.md", "needle DP\n");
    let record = fs::read_to_string(w.path("problems/atcoder-abc400_a/problem.toml")).unwrap();
    w.write(
        "problems/atcoder-abc400_a/problem.toml",
        format!("{record}title = 'Route'\nurl = 'https://example.invalid/problem'\n"),
    );
    for (terms, expected) in [
        (
            vec!["DP"],
            vec![
                "local:a",
                "atcoder:abc400_a",
                "local:c",
                "local:d",
                "local:g",
                "local:j",
                "local:wa",
            ],
        ),
        (vec!["é"], vec!["local:a", "local:b"]),
        (vec!["e\u{301}"], vec!["local:a", "local:b"]),
        (vec!["Route", "dp"], vec!["atcoder:abc400_a"]),
        (vec!["onlynotes"], vec!["local:a"]),
        (vec!["onlynotes", "--no-notes"], vec![]),
        (
            vec!["dp", "--status", "solved"],
            vec!["atcoder:abc400_a", "local:d"],
        ),
        (vec!["rated"], vec![]),
        (vec!["nothing-here"], vec![]),
    ] {
        let value = search_doc(&w, &terms);
        assert_eq!(hit_ids(&value), expected, "{terms:?}: {value}");
        // Invalid fields are reported even if the query/filter does not match.
        assert!(
            value["findings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["field"] == "tags")
        );
    }
    let value = search_doc(&w, &["e"]);
    assert!(hit_ids(&value).contains(&"local:a"));
    assert!(hit_ids(&value).contains(&"local:b"));
    let value = search_doc(&w, &["dp"]);
    let hit = value["hits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|hit| hit["problem"] == "local:d")
        .unwrap();
    assert_eq!(hit["fields"], serde_json::json!(["tags", "notes"]));
    assert_eq!(hit["notes_lines"], serde_json::json!([1]));
    let value = search_doc(&w, &["atcoder", "Route", "example.invalid", "dp", "solved"]);
    assert_eq!(
        value["hits"][0]["fields"],
        serde_json::json!(["identity", "title", "url", "tags", "status"])
    );
    let output = w.unchanged(&["search", "nothing-here"], 0);
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("no matches\n"));
    assert!(String::from_utf8_lossy(&output.stdout).contains("invalid_field"));
    // An absent status is effective `new` for filters, but isn't searched.
    assert!(
        search_doc(&w, &["new", "--status", "new"])["hits"]
            .as_array()
            .unwrap()
            .iter()
            .all(|hit| hit["problem"] != "local:a")
    );
    for (filter, value) in [
        ("--tag", "graphs"),
        ("--site", "atcoder"),
        ("--contest", "c1"),
        ("--created", "2026-11-24"),
        ("--updated", "2026-11-24"),
        ("--solved", "2026-11-24"),
        ("--archived-in", "2026-11-24"),
    ] {
        let listing = document(&w.unchanged(&["ls", filter, value, "--json"], 0));
        let search = search_doc(&w, &[":", filter, value]);
        let expected = listing["problems"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["problem"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(hit_ids(&search), expected);
    }
}

#[test]
fn search_line_numbers_truncation_and_lossy_notes() {
    let w = Workspace::new();
    for count in [16, 20] {
        w.write("problems/p/notes.md", "needle\r\n".repeat(count));
        let value = search_doc(&w, &["needle"]);
        assert_eq!(
            value["hits"][0]["notes_lines"],
            serde_json::json!((1..=16).collect::<Vec<_>>())
        );
        assert_eq!(value["hits"][0]["notes_lines_dropped"], count - 16);
        assert_eq!(value["incomplete"], false);
        let output = w.unchanged(&["search", "needle"], 0);
        assert!(!String::from_utf8_lossy(&output.stderr).contains("search incomplete"));
    }
    for size in [65_536, 65_537] {
        w.write("problems/p/notes.md", vec![b'x'; size]);
        let value = search_doc(&w, &["x"]);
        assert_eq!(value["hits"][0]["notes_truncated"], size > 65_536);
        assert_eq!(
            reasons(&value),
            if size > 65_536 {
                vec!["notes_truncated"]
            } else {
                vec![]
            }
        );
    }
    let mut bytes = vec![b'x'; 65_536];
    bytes.extend_from_slice(b"needle");
    w.write("problems/p/notes.md", &bytes);
    assert!(hit_ids(&search_doc(&w, &["needle"])).is_empty());
    let mut bytes = vec![b'x'; 65_535];
    bytes.extend_from_slice("€".as_bytes());
    w.write("problems/p/notes.md", &bytes);
    let value = search_doc(&w, &["€"]);
    assert!(hit_ids(&value).is_empty());
    assert!(value["findings"].as_array().unwrap().is_empty());
    w.write("problems/p/notes.md", b"\xffneedle");
    let value = search_doc(&w, &["needle"]);
    assert_eq!(hit_ids(&value), ["local:p"]);
    assert_eq!(value["findings"][0]["kind"], "notes_not_utf8");
    assert_eq!(value["incomplete"], false);
    // An earlier replacement must not hide a cut sequence at the cap.
    bytes[0] = 0xff;
    w.write("problems/p/notes.md", &bytes);
    let value = search_doc(&w, &["�"]);
    assert_eq!(value["findings"].as_array().unwrap().len(), 1);
}

#[test]
fn search_nonregular_and_unreadable_notes_are_visible_and_optional() {
    let w = Workspace::new();
    fs::create_dir(w.path("problems/p/notes.md")).unwrap();
    let value = search_doc(&w, &["nothing"]);
    assert_eq!(value["findings"][0]["kind"], "notes_not_regular");
    assert_eq!(reasons(&value), ["unreadable_source"]);
    let output = w.unchanged(&["search", "nothing"], 0);
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("no matches\n"));
    assert!(String::from_utf8_lossy(&output.stdout).contains("notes_not_regular"));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr).lines().last(),
        Some("search incomplete: unreadable_source")
    );
    assert_eq!(
        search_doc(&w, &["nothing", "--no-notes"])["incomplete"],
        false
    );
    fs::remove_dir(w.path("problems/p/notes.md")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::{PermissionsExt, symlink};
        w.write("target", "needle");
        symlink(w.path("target"), w.path("problems/p/notes.md")).unwrap();
        let value = search_doc(&w, &["needle"]);
        assert_eq!(value["findings"][0]["kind"], "notes_not_regular");
        assert_eq!(reasons(&value), ["unreadable_source"]);
        fs::remove_file(w.path("problems/p/notes.md")).unwrap();
        use std::os::unix::ffi::OsStrExt;
        let path =
            std::ffi::CString::new(w.path("problems/p/notes.md").as_os_str().as_bytes()).unwrap();
        // SAFETY: a valid NUL-terminated fixture path, user-only FIFO mode.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        let value = search_doc(&w, &["needle"]);
        assert_eq!(value["findings"][0]["kind"], "notes_not_regular");
        assert_eq!(reasons(&value), ["unreadable_source"]);
        fs::remove_file(w.path("problems/p/notes.md")).unwrap();
        w.write("problems/p/notes.md", "needle");
        let path = w.path("problems/p/notes.md");
        let original = fs::metadata(&path).unwrap().permissions();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
        if File::open(&path).is_err() {
            let value = document(&w.run(&["search", "needle", "--json"], 0));
            assert_eq!(value["findings"][0]["kind"], "notes_unreadable");
            assert_eq!(reasons(&value), ["unreadable_source"]);
            // Zero hits: `no matches`, the finding, then the incompleteness line.
            // `run`, not `unchanged`: a snapshot cannot read the mode-000 file.
            let output = w.run(&["search", "needle"], 0);
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(stdout.starts_with("no matches\n"), "{stdout}");
            assert!(stdout.contains("notes_unreadable"), "{stdout}");
            assert_eq!(
                String::from_utf8_lossy(&output.stderr).lines().last(),
                Some("search incomplete: unreadable_source")
            );
        }
        fs::set_permissions(path, original).unwrap();
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        w.write("problems/p/notes.md", "needle");
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(w.path("problems/p/notes.md"))
            .unwrap();
        // Exclusive sharing denies read handles without requiring elevation.
        let value = document(&w.run(&["search", "needle", "--json"], 0));
        assert_eq!(value["findings"][0]["kind"], "notes_unreadable");
        assert_eq!(reasons(&value), ["unreadable_source"]);
        assert_eq!(
            document(&w.run(&["search", "needle", "--no-notes", "--json"], 0))["incomplete"],
            false
        );
        drop(held);
        // Exercise the contract's deny-read ACL as well as sharing denial.
        let path = w.path("problems/p/notes.md");
        let deny = Command::new("icacls")
            .arg(&path)
            .args(["/deny", "*S-1-1-0:(R)"])
            .output()
            .unwrap();
        assert!(deny.status.success(), "{deny:?}");
        let output = w.run(&["search", "needle", "--json"], 0);
        let optional = w.run(&["search", "needle", "--no-notes", "--json"], 0);
        let restore = Command::new("icacls")
            .arg(&path)
            .args(["/remove:d", "*S-1-1-0"])
            .output()
            .unwrap();
        assert!(restore.status.success(), "{restore:?}");
        assert_eq!(document(&output)["findings"][0]["kind"], "notes_unreadable");
        assert_eq!(reasons(&document(&output)), ["unreadable_source"]);
        assert_eq!(document(&optional)["incomplete"], false);
        w.write("target", "needle");
        fs::remove_file(&path).unwrap();
        std::os::windows::fs::symlink_file(w.path("target"), &path).unwrap();
        assert_eq!(
            search_doc(&w, &["needle"])["findings"][0]["kind"],
            "notes_not_regular"
        );
    }
}

fn search_ten() -> Workspace {
    let w = Workspace::new();
    fs::remove_dir_all(w.path("problems")).unwrap();
    for index in 1..=10 {
        let mut record = format!("schema = 1\n[problem]\nid = 'local:p{index:02}'\n#");
        record.push_str(&" ".repeat(1000 - record.len()));
        w.write(&format!("problems/p{index:02}/problem.toml"), record);
        w.write(&format!("problems/p{index:02}/notes.md"), "x".repeat(2000));
    }
    w
}

#[test]
fn search_bounds_and_operation_accounting() {
    let w = search_ten();
    let logs = Directory::new();
    for (entries, bytes, no_notes, count, expected, used_e, used_b) in [
        (30, 30000, false, 10, vec![], 30, 30000),
        (24, 30000, false, 7, vec!["entry_limit"], 24, 21000),
        (5, 30000, false, 0, vec!["entry_limit"], 5, 0),
        (100, 30000, false, 10, vec![], 30, 30000),
        (100, 10000, false, 3, vec!["byte_limit"], 18, 10000),
        (100, 10000, true, 10, vec![], 20, 10000),
    ] {
        let used_e: u32 = used_e;
        let log = logs.path("calls");
        let _ = fs::remove_file(&log);
        let before = snapshot(&w.0.0);
        let mut command = w.command(&["search", "p", "--json"]);
        if no_notes {
            command.arg("--no-notes");
        }
        let output = command
            .env("CPF_STORAGE_SEARCH_ENTRIES", entries.to_string())
            .env("CPF_STORAGE_SEARCH_BYTES", bytes.to_string())
            .env("CPF_STORAGE_SEARCH_IO", &log)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let value = document(&output);
        assert_eq!(
            hit_ids(&value),
            (1..=count)
                .map(|i| format!("local:p{i:02}"))
                .collect::<Vec<_>>()
        );
        assert_eq!(reasons(&value), expected);
        assert_eq!(value["entries"], used_e);
        assert_eq!(value["bytes_read"], used_b.to_string());
        let calls = fs::read_to_string(&log).unwrap();
        let mut charged = std::collections::BTreeSet::new();
        let mut actual_bytes = 0_u64;
        for line in calls.lines() {
            let columns = line.split('\t').collect::<Vec<_>>();
            let action = columns[0].parse::<u32>().unwrap();
            assert!((1..=used_e).contains(&action));
            assert!(matches!(
                columns[1],
                "enumerate"
                    | "enumeration_end"
                    | "entry"
                    | "entry_type"
                    | "open"
                    | "handle_metadata"
                    | "read"
                    | "handle_length"
            ));
            if matches!(columns[1], "entry" | "open") {
                assert!(charged.insert(action));
            }
            actual_bytes += columns[3].parse::<u64>().unwrap();
        }
        assert_eq!(charged.len(), used_e as usize);
        assert_eq!(actual_bytes.to_string(), value["bytes_read"]);
        assert_eq!(snapshot(&w.0.0), before);
    }
    // First candidate truncates notes; the second record exceeds B.
    w.write("problems/p01/notes.md", "x".repeat(65_537));
    let output = w
        .command(&["search", "p", "--json"])
        .env("CPF_STORAGE_SEARCH_BYTES", "66537")
        .output()
        .unwrap();
    assert_eq!(
        reasons(&document(&output)),
        ["byte_limit", "notes_truncated"]
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stderr).lines().last(),
        Some("search incomplete: byte_limit, notes_truncated")
    );
}

#[test]
fn search_growth_never_exceeds_byte_allowance() {
    let w = search_ten();
    let mut child = w
        .command(&["search", "p", "--no-notes", "--json"])
        .env("CPF_STORAGE_SEARCH_BYTES", "1000")
        .env("CPF_STORAGE_PAUSE_AT", "search.read#1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    barrier(&mut child, "search.read#1");
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(w.path("problems/p01/problem.toml"))
        .unwrap();
    file.write_all(&[b' '; 1000]).unwrap();
    drop(file);
    release(&mut child);
    let value = document(&finished(child, 0));
    assert_eq!(value["bytes_read"], "1000");
    assert!(hit_ids(&value).is_empty());
    assert_eq!(reasons(&value), ["byte_limit"]);
}

#[cfg(target_os = "linux")]
#[test]
fn search_real_entry_limit_and_collision_inventory() {
    let w = Workspace::new();
    fs::remove_dir_all(w.path("problems")).unwrap();
    fs::create_dir(w.path("problems")).unwrap();
    for i in 0..100_001 {
        fs::create_dir(w.path(&format!("problems/p{i}"))).unwrap();
    }
    let logs = Directory::new();
    let log = logs.path("calls");
    let recorded = |args: &[&str]| {
        let _ = fs::remove_file(&log);
        let output = w
            .command(args)
            .env("CPF_STORAGE_SEARCH_IO", &log)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        (document(&output), fs::read_to_string(&log).unwrap())
    };
    let (value, calls) = recorded(&["search", "x", "--json"]);
    assert_eq!(reasons(&value), ["entry_limit"]);
    assert_eq!(value["entries"], 100_000);
    assert_eq!(value["bytes_read"], "0");
    assert!(hit_ids(&value).is_empty());
    assert!(calls.lines().all(|line| !line.contains("\topen\t")));
    fs::remove_dir_all(w.path("problems")).unwrap();
    for name in ["P1", "p1"] {
        w.write(
            &format!("problems/{name}/problem.toml"),
            format!("[problem]\nid='local:{name}'\n"),
        );
    }
    let (value, calls) = recorded(&["search", "x", "--json"]);
    assert_eq!(value["entries"], 2);
    let kinds = value["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["path"]["utf8"].as_str().unwrap(),
                f["kind"].as_str().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        [
            ("problems/P1", "name_collision"),
            ("problems/p1", "name_collision")
        ]
    );
    let enumerations = calls
        .lines()
        .filter(|line| line.split('\t').nth(1) == Some("enumerate"))
        .count();
    assert_eq!(enumerations, 1, "{calls}");
}

#[test]
fn search_classifies_oversized_records_and_linked_inventory_like_ls() {
    let w = Workspace::new();
    let mut record = BASE.to_owned();
    record.push('#');
    record.push_str(&" ".repeat(1024 * 1024 + 1 - record.len()));
    w.write("problems/p/problem.toml", record);
    let value = search_doc(&w, &["p"]);
    assert!(hit_ids(&value).is_empty());
    assert_eq!(value["findings"][0]["kind"], "metadata_malformed");
    // Classified from the handle length: nothing is read.
    assert_eq!(value["bytes_read"], "0");
    let listing = document(&w.unchanged(&["ls", "--json"], 0));
    assert_eq!(listing["invalid"][0]["kind"], "metadata_malformed");
    #[cfg(unix)]
    {
        w.write(
            "elsewhere/q/problem.toml",
            "schema = 1\n[problem]\nid = 'local:q'\n",
        );
        fs::remove_dir_all(w.path("problems")).unwrap();
        std::os::unix::fs::symlink("elsewhere", w.path("problems")).unwrap();
        w.unchanged(&["ls", "--json"], 8);
        let value = document(&w.unchanged(&["search", "q", "--json"], 8));
        assert_eq!(value["document"], "error");
    }
}

#[test]
fn search_never_enumerates_unrequested_contests() {
    let w = Workspace::new();
    for i in 0..100_001 {
        fs::create_dir_all(w.path(&format!("contests/c{i}"))).unwrap();
    }
    let logs = Directory::new();
    let path = logs.path("calls");
    let output = w
        .command(&["search", "p", "--json"])
        .env("CPF_STORAGE_SEARCH_IO", &path)
        .output()
        .unwrap();
    assert!(output.status.success());
    let value = document(&output);
    assert_eq!(value["incomplete"], false);
    assert!(!fs::read_to_string(path).unwrap().contains("contests"));
    let value = search_doc(&w, &["p", "--contest", "c1"]);
    assert_eq!(reasons(&value), ["entry_limit"]);
    assert_eq!(value["entries"], 100_000);
    assert_eq!(value["bytes_read"], "0");
    assert!(hit_ids(&value).is_empty());
}

#[test]
fn readers_use_torn_bytes_once_for_metadata_and_notes() {
    let w = Workspace::new();
    // Equal lengths: old first half + new second half is malformed TOML.
    let old = "[problem]\nid = 'local:p'\n#padding padding padding\n";
    let mut new = vec![b' '; old.len()];
    new[old.len() / 2] = b'\'';
    w.write("problems/p/problem.toml", old);
    let mut child = paused(&w, &["ls", "--json"], "org.read_partial");
    w.write("problems/p/problem.toml", &new);
    release(&mut child);
    assert_eq!(
        document(&finished(child, 0))["invalid"][0]["kind"],
        "metadata_malformed"
    );
    w.write("problems/p/problem.toml", BASE);
    // Avoid the metadata partial-read hook; select its second occurrence by
    // releasing the record read, then rewrite the notes at the next pause.
    w.write("problems/p/notes.md", "oldoldoldold");
    let mut child = paused(
        &w,
        &["search", "oldold", "newnew", "--json"],
        "org.read_partial",
    );
    release(&mut child);
    barrier(&mut child, "org.read_partial");
    w.write("problems/p/notes.md", "newnewnewnew");
    release(&mut child);
    let value = document(&finished(child, 0));
    assert_eq!(hit_ids(&value), ["local:p"]);
    assert_eq!(value["incomplete"], false);
}

fn signal_command(w: &Workspace, args: &[&str]) -> Command {
    let mut command = w.command(args);
    new_process_group(&mut command);
    command
}
#[cfg(windows)]
fn new_process_group(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::{Console::AllocConsole, Threading::CREATE_NEW_PROCESS_GROUP};
    // A console may already be attached. Each child gets a separate group.
    // SAFETY: no arguments; failure only means a console is already attached.
    unsafe {
        AllocConsole();
    }
    command.creation_flags(CREATE_NEW_PROCESS_GROUP);
}
#[cfg(not(windows))]
fn new_process_group(_: &mut Command) {}
fn search_signal(child: &Child) {
    #[cfg(unix)]
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent};
        assert_ne!(
            unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, child.id()) },
            0
        );
    }
}
fn signal_exit(output: &Output) {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(output.status.signal(), Some(libc::SIGINT), "{output:?}");
    }
    #[cfg(windows)]
    assert_eq!(output.status.code(), Some(6), "{output:?}");
}

#[test]
fn search_cancellation_abandons_worker_and_delivers_finished_candidates() {
    let w = search_ten();
    for (point, count) in [("search.before_action#20", 4), ("search.read#5", 0)] {
        let before = snapshot(&w.0.0);
        let mut child = signal_command(&w, &["search", "p", "--json"])
            .env("CPF_STORAGE_PAUSE_AT", point)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        barrier(&mut child, point);
        let start = std::time::Instant::now();
        search_signal(&child);
        let mut line = String::new();
        BufReader::new(child.stdout.as_mut().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert!(start.elapsed() < std::time::Duration::from_millis(200));
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            hit_ids(&value),
            (1..=count)
                .map(|i| format!("local:p{i:02}"))
                .collect::<Vec<_>>()
        );
        assert_eq!(reasons(&value), ["cancelled"]);
        // Keep the barrier pipe open: the paused worker is abandoned, not released.
        let _stdin = child.stdin.take();
        let output = child.wait_with_output().unwrap();
        signal_exit(&output);
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
        assert_eq!(snapshot(&w.0.0), before);
    }
}

#[test]
fn search_second_signal_forces_exit_during_stalled_delivery() {
    let w = search_ten();
    let signals = Directory::new();
    let ready = signals.path("ready");
    let mut child = signal_command(&w, &["search", "p", "--json"])
        .env("CPF_STORAGE_PAUSE_AT", "search.stdout")
        .env("CPF_STORAGE_SIGNAL_READY", &ready)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    barrier(&mut child, "search.stdout");
    search_signal(&child);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    while !ready.exists() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    std::thread::sleep(std::time::Duration::from_millis(110));
    let start = std::time::Instant::now();
    search_signal(&child);
    let output = child.wait_with_output().unwrap();
    signal_exit(&output);
    assert!(start.elapsed() < std::time::Duration::from_secs(1));
}

#[test]
fn search_exit_statuses_and_missing_inventory() {
    let outside = Directory::new();
    let output = Command::new(env!("CARGO_BIN_EXE_cpf"))
        .current_dir(&outside.0)
        .args(["search", "x", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(8));
    assert_eq!(document(&output)["document"], "error");
    assert_eq!(document(&output)["command"], "search");
    let w = Workspace::new();
    fs::remove_dir_all(w.path("problems")).unwrap();
    let value = search_doc(&w, &["x"]);
    assert_eq!(value["incomplete"], false);
    assert!(hit_ids(&value).is_empty());
    w.write("contests/broken/contest.toml", "[");
    for name in ["missing", "broken"] {
        w.unchanged(&["search", "x", "--contest", name, "--json"], 8);
    }
    for (args, code) in [
        (vec!["search", "x", "--json"], 11),
        (vec!["search", "--json"], 2),
    ] {
        let mut child = w
            .command(&args)
            .stdout(closed_pipe())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        assert_eq!(child.wait().unwrap().code(), Some(code));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::create_dir(w.path("problems")).unwrap();
        let path = w.path("problems");
        let original = fs::metadata(&path).unwrap().permissions();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read_dir(&path).is_err() {
            w.run(&["search", "x", "--json"], 8);
        }
        fs::set_permissions(path, original).unwrap();
    }
}

#[test]
fn cancelled_search_with_broken_delivery_exits_eleven() {
    let w = search_ten();
    let mut child = signal_command(&w, &["search", "p", "--json"])
        .env("CPF_STORAGE_PAUSE_AT", "search.before_action#20")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    barrier(&mut child, "search.before_action#20");
    drop(child.stdout.take());
    search_signal(&child);
    // Keep the barrier pipe open: the paused worker is abandoned, not released.
    let _stdin = child.stdin.take();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(11), "{output:?}");
}

#[test]
fn search_human_cancellation_reports_only_completed_hits() {
    let w = search_ten();
    let before = snapshot(&w.0.0);
    let mut child = signal_command(&w, &["search", "p"])
        .env("CPF_STORAGE_PAUSE_AT", "search.before_action#20")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    barrier(&mut child, "search.before_action#20");
    search_signal(&child);
    // Keep the barrier pipe open: the paused worker is abandoned, not released.
    let _stdin = child.stdin.take();
    let output = child.wait_with_output().unwrap();
    signal_exit(&output);
    let text = String::from_utf8_lossy(&output.stdout);
    assert_eq!(text.lines().count(), 4);
    for i in 1..=4 {
        assert!(text.contains(&format!("local:p{i:02}")));
    }
    assert!(!text.contains("local:p05"));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr).lines().last(),
        Some("search incomplete: cancelled")
    );
    assert_eq!(snapshot(&w.0.0), before);
}
