use cpf_fixture::harness::{Fixture, Options, fixture_path};
use std::time::Duration;

#[test]
fn root_tests_can_discover_and_use_the_workspace_fixture() {
    let expected = std::env::var_os("CPF_FIXTURE_BIN")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            // Both packages' ordinary binaries share Cargo's output directory.
            std::path::Path::new(env!("CARGO_BIN_EXE_cpf"))
                .with_file_name(format!("cpf-fixture{}", std::env::consts::EXE_SUFFIX))
        });
    assert_eq!(fixture_path(), expected);
    assert!(
        fixture_path().is_file(),
        "build fixtures with cargo test --workspace"
    );
    let mut fixture = Fixture::spawn(
        &["ready", "exit:0"],
        Options {
            deadline: Duration::from_secs(8),
            ..Options::default()
        },
    );
    assert_eq!(fixture.await_ready(1)[0].pid, fixture.id());
    assert!(fixture.wait().unwrap().success());
}
