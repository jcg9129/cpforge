//! Parsing and unavailable-command refusals must be portable and perform no I/O.
use std::process::{Command, Output};

const FAILURE: &str = "f-0123456789abcdef0123456789abcdef-0-0";

fn cpf(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cpf"))
        .args(args)
        .output()
        .expect("cpf should launch")
}

fn forms() -> Vec<Vec<&'static str>> {
    vec![
        vec![
            "fetch",
            "https://atcoder.jp/contests/abc300",
            "--only",
            "A",
            "--only",
            "B",
            "--disable-removed",
            "--offline",
            "--json",
        ],
        vec![
            "fetch",
            "https://cses.fi/problemset/task/1068",
            "--all-in-section",
        ],
        vec![
            "fetch",
            "http://atcoder.jp/contests/abc300/tasks/?lang=en",
            "--only",
            "A",
        ],
        vec!["fetch", "contest:abc300", "--only", "A"],
        vec![
            "fetch",
            "https://cses.fi/problemset/view/1068",
            "--all-in-section",
        ],
        vec!["fetch", "--history"],
        vec!["fetch", "--history", "atcoder:abc300_a"],
        vec![
            "ls",
            "--archived",
            "--contests",
            "--root",
            "snapshots",
            "--json",
        ],
        vec![
            "search",
            "graph",
            "path",
            "--no-notes",
            "--no-archives",
            "--root",
            "snapshots",
            "--json",
        ],
        vec![
            "stress",
            "--profile",
            "p",
            "--deadline",
            "2s",
            "--seed",
            "0",
            "--iterations",
            "10",
            "--time",
            "5m",
            "--keep-going",
            "--events=output",
        ],
        vec![
            "stress",
            "--replay",
            FAILURE,
            "--profile",
            "p",
            "--deadline",
            "1s",
            "--json",
        ],
        vec!["stress", "--replay-binaries", FAILURE, "--events"],
        vec!["stress", "ls", "--json"],
        vec!["test", "add", "--from-stress", FAILURE],
        vec![
            "interactor-test",
            "--case",
            "a",
            "--case",
            "b",
            "--profile",
            "p",
            "--deadline",
            "1s",
        ],
        vec![
            "submit-prep",
            "--source-copy",
            "--no-test",
            "--copy",
            "--json",
        ],
        vec![
            "submit-prep",
            "--profile",
            "p",
            "--judge",
            "atcoder",
            "--no-copy",
            "--events",
        ],
        vec![
            "archive",
            ".",
            "--to",
            "snapshots",
            "--keep-profile",
            "p",
            "--cleanup",
            "quarantine",
            "--on-existing",
            "new",
            "--apply",
            "--plan",
            "hash",
            "--json",
        ],
        vec!["archive", "--resume", "--apply", "--json"],
        vec!["archive", "--close", "txn", "--apply"],
        vec!["quarantine", "ls"],
        vec!["quarantine", "purge", "a", "b", "--apply"],
        vec![
            "restore",
            "snapshot",
            "--into",
            "copy",
            "--partial",
            "--root",
            "one",
            "--root",
            "two",
            "--json",
        ],
    ]
}

#[test]
fn help_lists_all_commands_and_each_command_has_help() {
    let output = cpf(&["--help"]);
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for name in [
        "fetch",
        "tag",
        "untag",
        "mark",
        "notes",
        "ls",
        "search",
        "status",
        "stress",
        "test",
        "interactor-test",
        "submit-prep",
        "archive",
        "quarantine",
        "restore",
        "clean",
        "config",
        "doctor",
        "cmd",
        "build",
        "run",
        "adopt",
        "ignore",
        "fsck",
        "gitignore",
        "current",
        "switch",
        "add",
        "init",
    ] {
        assert!(
            help.lines()
                .any(|line| line.split_whitespace().next() == Some(name)),
            "missing {name}: {help}"
        );
        assert!(cpf(&[name, "--help"]).status.success(), "{name}");
    }
}

#[test]
fn unfinished_forms_refuse_and_invalid_flags_are_clap_usage_errors() {
    for mut args in forms() {
        let output = cpf(&args);
        assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        let name = if args.starts_with(&["test", "add"]) {
            "test add --from-stress"
        } else if args.starts_with(&["stress", "ls"]) {
            "stress ls"
        } else if args.starts_with(&["ls", "--archived"]) {
            "ls --archived"
        } else if args[0] == "search" {
            // Workspace search has landed; only the archive-root form has not.
            "search --root"
        } else if args[0] == "quarantine" {
            if args[1] == "ls" {
                "quarantine ls"
            } else {
                "quarantine purge"
            }
        } else {
            args[0]
        };
        assert!(
            stderr.contains(&format!("cpf {name}: not available yet")),
            "{args:?}: {stderr}"
        );
        args.push("--invalid-surface-flag");
        let invalid = cpf(&args);
        assert_eq!(invalid.status.code(), Some(2), "{args:?}");
        let stderr = String::from_utf8_lossy(&invalid.stderr);
        assert!(
            stderr.contains("unexpected argument") && stderr.contains("Usage:"),
            "{args:?}: {stderr}"
        );
        assert!(!stderr.contains("not available yet"));
    }
}

#[test]
fn invalid_values_and_combinations_are_usage_errors() {
    for args in [
        vec!["fetch"],
        vec!["fetch", "--history", "--json"],
        vec!["tag", "p"],
        vec!["tag", "p", "bad tag"],
        vec!["mark", "p", "bad_status"],
        vec!["ls", "--contests", "--status", "solved"],
        vec!["ls", "--site", "AtCoder"],
        vec!["ls", "--contest", "a..b"],
        vec!["status", "--created", "2026-02-30"],
        vec!["search", ""],
        vec!["stress", "--iterations", "0"],
        vec!["stress", "--seed", "-1"],
        vec!["stress", "--seed", "+1"],
        vec!["stress", "--seed", "01"],
        vec!["stress", "--time", "0ms"],
        vec!["stress", "--replay", "bad"],
        vec!["stress", "--replay", FAILURE, "--replay-binaries", FAILURE],
        vec!["stress", "--replay", FAILURE, "--keep-going"],
        vec!["stress", "--replay-binaries", FAILURE, "--profile", "p"],
        vec!["stress", "ls", "--events"],
        vec!["test", "add", "--from-stress", FAILURE, "--input", "input"],
        vec![
            "test",
            "add",
            "--from-stress",
            FAILURE,
            "--answer",
            "answer",
        ],
        vec!["interactor-test", "--json"],
        vec!["interactor-test", "--events"],
        vec!["submit-prep", "--copy", "--no-copy"],
        vec!["submit-prep", "--source-copy", "--events"],
        vec!["submit-prep", "--events=output"],
        vec!["archive"],
        vec!["archive", ".", "--plan", "hash"],
        vec!["archive", "--resume", "."],
        vec!["archive", "--close", "txn", "--keep-profile", "p"],
        vec!["archive", "--close", "txn", "--json"],
        vec!["archive", ".", "--cleanup", "bad"],
        vec!["quarantine", "ls", "--json"],
        vec!["restore"],
        vec!["clean", "--json"],
    ] {
        let output = cpf(&args);
        assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Clap reports a bad value without the usage block; both are usage errors.
        assert!(
            stderr.contains("Usage:") || stderr.contains("invalid value"),
            "{args:?}: {stderr}"
        );
        assert!(!stderr.contains("not available yet"), "{args:?}: {stderr}");
    }
}

#[test]
fn unfinished_commands_create_nothing() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join(format!(
            "surface-refusals-{}",
            cpforge::build::BuildId::generate().unwrap()
        ));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("cpforge.toml"), b"schema = 1\n").unwrap();
    for args in forms() {
        let output = Command::new(env!("CARGO_BIN_EXE_cpf"))
            .current_dir(&root)
            .args(&args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
        let entries: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(
            entries,
            [std::ffi::OsString::from("cpforge.toml")],
            "{args:?}"
        );
        assert_eq!(
            std::fs::read(root.join("cpforge.toml")).unwrap(),
            b"schema = 1\n"
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn global_selector_and_target_only_flags_conflict_before_refusal() {
    for args in [
        vec!["--problem", "p", "fetch", "p"],
        vec!["--problem", "p", "archive", "."],
        vec!["--problem", "p", "restore", "snapshot"],
        vec!["--problem", "p", "quarantine", "ls"],
        vec!["--problem", "p", "tag", ".", "dp"],
        vec!["--problem", "p", "untag", ".", "dp"],
        vec!["--problem", "p", "mark", ".", "solved"],
        vec!["--problem", "p", "notes", "p"],
        vec!["mark", "contest:c", "solved"],
        vec!["notes", "contest:c"],
        vec!["fetch", "p", "--only", "A"],
        vec!["fetch", "p", "--all-in-section"],
    ] {
        let output = cpf(&args);
        assert_eq!(output.status.code(), Some(2));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("Usage:"), "{args:?}: {stderr}");
        assert!(!stderr.contains("not available yet"), "{args:?}: {stderr}");
    }
}
