use std::process::Command;

#[test]
fn unavailable_machine_modes_and_conflicting_flags_are_usage_errors() {
    for args in [
        vec!["test", "--json", "--events"],
        vec!["run", "--json"],
        vec!["run", "--events"],
        vec!["current", "--json"],
        vec!["switch", "one", "--events"],
        vec!["test", "ls", "--json"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_cpf"))
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .args(&args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
        assert!(output.stdout.is_empty(), "{args:?}: {output:?}");
    }
}

#[test]
fn test_job_entry_delivers_preflight_refusals_in_machine_modes() {
    for mode in ["--json", "--events", "--events=output"] {
        let output = Command::new(env!("CARGO_BIN_EXE_cpf"))
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .args(["test", mode])
            .output()
            .unwrap();
        let supported = cfg!(any(target_os = "linux", target_os = "macos"));
        assert_eq!(
            output.status.code(),
            Some(if supported { 8 } else { 10 }),
            "{output:?}"
        );
        assert_eq!(output.stdout.last(), Some(&b'\n'));
        let lines: Vec<serde_json::Value> = output
            .stdout
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect();
        let report = if mode == "--json" {
            assert_eq!(lines.len(), 1);
            &lines[0]
        } else {
            assert_eq!(lines.len(), 2);
            assert_eq!(lines[0]["document"], "event_stream");
            assert_eq!(lines[0]["output"], mode == "--events=output");
            assert_eq!(lines[1]["event"], "report");
            &lines[1]["report"]
        };
        assert_eq!(report["document"], "job_report");
        assert_eq!(report["outcome"], "refused");
        assert_eq!(
            report["refusal"]["class"],
            if supported {
                "workspace"
            } else {
                "infrastructure"
            }
        );
        assert_eq!(report["persistence"]["report"]["kind"], "not_attempted");
    }
}

#[cfg(unix)]
#[test]
fn stdout_closed_at_start_is_delivery_failure_before_the_job_entry() {
    use std::os::unix::process::CommandExt;
    let mut command = Command::new(env!("CARGO_BIN_EXE_cpf"));
    command
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(["test", "--json"]);
    // SAFETY: the child callback only closes its standard stdout descriptor.
    // cpf repairs it before any files can reuse fd 1.
    unsafe {
        command.pre_exec(|| {
            libc::close(1);
            Ok(())
        });
    }
    let output = command.output().unwrap();
    assert_eq!(output.status.code(), Some(11), "{output:?}");
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("job not admitted"));
}
