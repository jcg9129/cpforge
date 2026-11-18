use std::process::{Command, Output};

fn cpf(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cpf"))
        .args(args)
        .output()
        .expect("cpf should launch")
}

#[test]
fn help_succeeds() {
    for flag in ["--help", "-h"] {
        let output = cpf(&[flag]);
        assert_eq!(output.status.code(), Some(0));
        assert!(String::from_utf8_lossy(&output.stdout).contains("cpf"));
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn version_matches_package() {
    let output = cpf(&["--version"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        concat!("cpf ", env!("CARGO_PKG_VERSION"))
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn missing_command_prints_help_to_stderr() {
    let output = cpf(&[]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Usage: cpf"));
    assert!(stderr.contains("--help"));
}

#[test]
fn unknown_flag_is_a_usage_error() {
    let output = cpf(&["--unknown"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.starts_with("cpf: error: "));
    assert!(stderr.contains("--unknown"));
}

#[test]
fn color_policies_are_accepted_but_still_require_a_command() {
    for color in ["auto", "always", "never"] {
        let output = cpf(&["--color", color]);
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains("Usage: cpf"));
    }
}

#[test]
fn invalid_color_is_a_usage_error() {
    let output = cpf(&["--color", "invalid"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid value"));
}
