# cpf-fixture

Test-only step scripts and a small harness, independent of the cpforge supervisor.
The crate is unpublished and is a dev-dependency of the root package.

`cargo test --workspace --all-targets` builds the binary before tests run: this
crate's integration test references `env!("CARGO_BIN_EXE_cpf-fixture")`, which
instructs Cargo to build the ordinary binary as well as its test target.
`cpf_fixture::harness::fixture_path()` is the helper for root-package tests too.
It uses `CPF_FIXTURE_BIN` when set, otherwise the current test executable's
profile directory: a test at `target/debug/deps/test-HASH` finds
`target/debug/cpf-fixture` (`.exe` on Windows). Custom target directories,
release profiles, and target-triple subdirectories work the same way.
For a root-only test invocation on a clean checkout, first run
`cargo build -p cpf-fixture --bin cpf-fixture`; alternatively set
`CPF_FIXTURE_BIN` to an explicitly built binary's absolute path. No recursive
Cargo invocation or build-script side effects are needed.

```rust,no_run
use cpf_fixture::harness::{Fixture, Options, assert_all_gone};
use std::time::Duration;

let mut fixture = Fixture::spawn(
    &["ready", "spawn:2:ready,hold", "hold"], Options::default(),
);
let identities = fixture.await_ready(3);
fixture.kill_tree().unwrap();
assert_all_gone(&identities, Duration::from_secs(2));
fixture.wait().unwrap();
```

Each argv element is a step. Validate the entire script, including descendants,
before executing anything. `write:stdout:10:z` writes ten ASCII `z` bytes; the
optional byte must be exactly one UTF-8 byte. Unsigned sizes and times are decimal.
Spawn sub-steps are comma-separated; one nested spawn uses semicolons for its
own sub-steps. For example, pass one argument
`spawn:2:ready,spawn:1:ready;hold,hold` to create two children, each creating one
grandchild. The harness can collect five identities when the root also executes
`ready`. A second level of nested spawn is rejected. Colons, commas, and
semicolons are reserved syntax; there is no general-purpose backslash escaping.
Quote spawn arguments in a shell so the shell does not interpret semicolons.

The control channel is test-only, outside the supervisor's descriptor inheritance
rules. On Unix, `CPF_FIXTURE_CTRL` and `CPF_FIXTURE_ACK` name the same inherited
Unix datagram socket, half of a socketpair. Only that endpoint loses close-on-exec
in the child, and descendants inherit it. Datagrams preserve READY/ACK boundaries.
On Windows, the variables contain decimal inherited handle values for the write
and read ends of two anonymous pipes. Each READY line is written in one bounded
write; the single harness reader assembles lines. All protocol messages are at
most 256 bytes.

`ready` writes `READY <pid> <start-id>\n` and blocks. The harness checks the
identity (and holds an opened process handle on Windows) before writing
`ACK <pid>\n`. Every waiting process **peeks** at the next ACK, but only its
addressee consumes it. Thus descendants, including scripts with repeated ready
steps, cannot steal acknowledgements. An ACK at the head of the queue waits for
its addressee to run. Each handshake direction has a five-second timeout; a
fixture waiting too long exits 65, and a harness readiness failure kills
containment and panics. `ready` without the channel is a validation error (64).

The watchdog owns an independent containment reference and timer. It kills the
Unix process group or Windows Job Object even while the test blocks in a pipe
read or `wait`. Windows uses a kill-on-close Job Object; an inherited event gates
the root before script execution until job assignment succeeds. Descendants
inherit the signalled event and job membership. This test-specific startup gate
is not the supervisor's process-creation implementation.

`Fixture::wait`/`try_wait` report watchdog expiry as `io::ErrorKind::TimedOut`.
Drop kills containment, reaps the root, and joins the timer thread. An expiry
which was not reported by a wait fails the test on Drop (unless already
unwinding). This permits an explicit expected-watchdog test without hiding
accidental timeouts. The default deadline is 30 seconds; the fixture tests use
eight seconds, except the one-second watchdog tests.

`Identity::is_live` treats a recycled PID or Unix zombie as gone. It conservatively
treats inaccessible identities as live. `assert_all_gone` asserts no live
identity, not disappearance of orphan zombie entries; the harness is not a Linux
subreaper. Unix `escape` calls `setsid`, so an escaped descendant survives a
process-group kill; tests using it must kill that descendant explicitly by PID.
The escape test demonstrates this containment limitation shared by the future
supervisor. The harness does not implement supervisor observation or accounting.

Additional connected-session steps: `read-exactly:N` consumes exactly N stdin
bytes (an early EOF fails the script), and Unix `default-sigpipe` restores
SIGPIPE's default disposition so a later broken output pipe terminates the
fixture by SIGPIPE. These steps compose with `write`, `close`, and `hold`.
