use cpf_fixture::harness::{Fixture, Options, fixture_path};
use std::time::Duration;

#[test]
fn root_tests_can_discover_and_use_the_workspace_fixture() {
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
