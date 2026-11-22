use cpf_fixture::harness::fixture_path;
use cpforge::supervisor::{ChildEnd, ParentReader, ReadResult, stdout_pipe};
use std::future::Future;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Mutex;
use std::time::Duration;

// Lock before creating each runtime, and destroy it before unlocking. This
// makes the /proc/self/fd checks quiescent even with the default test runner.
static TEST_LOCK: Mutex<()> = Mutex::new(());

fn run_test(test: impl Future<Output = ()>) {
    let _lock = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            tokio::time::timeout(Duration::from_secs(10), test)
                .await
                .expect("pipe test exceeded 10 seconds");
        });
}

// The fixture harness fixes stdout to Stdio::piped(). These tests instead use its
// fixture locator with our own stdio and a guard that reaps on timeout/panic.
struct TestChild(Child);

impl TestChild {
    fn spawn(steps: &[&str], stdout: Stdio, stderr: Stdio) -> Self {
        Self(
            Command::new(fixture_path())
                .args(steps)
                .stdin(Stdio::null())
                .stdout(stdout)
                .stderr(stderr)
                .env_remove("CPF_FIXTURE_CTRL")
                .env_remove("CPF_FIXTURE_ACK")
                .env_remove("CPF_FIXTURE_GATE")
                .spawn()
                .expect("spawn cpf-fixture (build with cargo test --workspace)"),
        )
    }

    async fn wait(&mut self) -> ExitStatus {
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
}

impl Drop for TestChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn child_stdio(child: ChildEnd) -> Stdio {
    #[cfg(unix)]
    {
        child.into_stdio()
    }
    #[cfg(windows)]
    {
        child.into_stdio_for_tests().unwrap()
    }
}

async fn drain(reader: &mut ParentReader, buffer_size: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut buf = vec![0; buffer_size];
    loop {
        match reader.read_chunk(&mut buf).await {
            ReadResult::Data(n) => {
                assert!((1..=buffer_size.min(64 * 1024)).contains(&n));
                bytes.extend_from_slice(&buf[..n]);
            }
            ReadResult::Eof => return bytes,
            ReadResult::Failed(error) => panic!("pipe read failed: {error}"),
        }
    }
}

#[test]
fn stdout_and_stderr_deliver_200000_bytes_in_bounded_chunks_then_eof() {
    run_test(async {
        for stream in ["stdout", "stderr"] {
            let mut pipe = stdout_pipe().unwrap();
            let stdio = child_stdio(pipe.child);
            let (stdout, stderr) = if stream == "stdout" {
                (stdio, Stdio::null())
            } else {
                (Stdio::null(), stdio)
            };
            let mut child = TestChild::spawn(&[&format!("write:{stream}:200000")], stdout, stderr);
            assert_eq!(
                drain(&mut pipe.parent, 128 * 1024).await,
                vec![b'x'; 200000]
            );
            assert!(child.wait().await.success());
        }
    });
}

#[test]
fn eof_waits_for_descendant_writer_after_root_exit() {
    run_test(async {
        let mut pipe = stdout_pipe().unwrap();
        let mut child = TestChild::spawn(
            &["spawn:1:sleep:300,write:stdout:5", "exit:0"],
            child_stdio(pipe.child),
            Stdio::null(),
        );
        assert!(child.wait().await.success());
        // Root is already reaped, but its descendant still owns the writer.
        assert_eq!(drain(&mut pipe.parent, 3).await, b"xxxxx");
    });
}

#[test]
fn empty_output_is_eof_even_if_writer_closes_before_first_read() {
    run_test(async {
        let mut pipe = stdout_pipe().unwrap();
        let mut child = TestChild::spawn(&["exit:0"], child_stdio(pipe.child), Stdio::null());
        assert!(child.wait().await.success());
        assert!(matches!(
            pipe.parent.read_chunk(&mut [0; 1]).await,
            ReadResult::Eof
        ));
    });
}

#[test]
fn empty_buffer_is_rejected_without_consuming_data() {
    run_test(async {
        let mut pipe = stdout_pipe().unwrap();
        let mut child =
            TestChild::spawn(&["write:stdout:5"], child_stdio(pipe.child), Stdio::null());
        match pipe.parent.read_chunk(&mut []).await {
            ReadResult::Failed(error) => assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput),
            result => panic!("empty buffer returned {result:?}"),
        }
        assert_eq!(drain(&mut pipe.parent, 1).await, b"xxxxx");
        assert!(child.wait().await.success());
    });
}

#[test]
fn dropping_an_unspawned_writer_produces_eof() {
    run_test(async {
        let mut pipe = stdout_pipe().unwrap();
        drop(pipe.child);
        assert!(matches!(
            pipe.parent.read_chunk(&mut [0; 1]).await,
            ReadResult::Eof
        ));
    });
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::PathBuf;

    fn fd_count() -> usize {
        fs::read_dir("/proc/self/fd").unwrap().count()
    }

    fn pipe_inodes(pid: u32) -> BTreeSet<PathBuf> {
        fs::read_dir(format!("/proc/{pid}/fd"))
            .unwrap()
            .map(|entry| fs::read_link(entry.unwrap().path()).unwrap())
            .filter(|target| target.to_string_lossy().starts_with("pipe:["))
            .collect()
    }

    async fn create_use_drop() {
        let mut pipe = stdout_pipe().unwrap();
        let mut child =
            TestChild::spawn(&["write:stdout:1"], child_stdio(pipe.child), Stdio::null());
        assert_eq!(drain(&mut pipe.parent, 1).await, b"x");
        assert!(child.wait().await.success());
    }

    #[test]
    fn no_fd_leak_after_100_create_use_drop_cycles() {
        run_test(async {
            create_use_drop().await;
            tokio::task::yield_now().await;
            let before = fd_count();
            for _ in 0..100 {
                create_use_drop().await;
            }
            tokio::task::yield_now().await;
            assert_eq!(fd_count(), before);
        });
    }

    #[test]
    fn unrelated_concurrent_process_does_not_inherit_pipe() {
        run_test(async {
            let before = pipe_inodes(std::process::id());
            let pipe = stdout_pipe().unwrap();
            let after = pipe_inodes(std::process::id());
            let created: Vec<_> = after.difference(&before).collect();
            assert_eq!(created.len(), 1, "both pipe ends must share one inode");

            let child = TestChild::spawn(&["hold"], Stdio::null(), Stdio::null());
            assert!(!pipe_inodes(child.0.id()).contains(created[0]));
            drop(child);
            drop(pipe);
        });
    }
}

#[cfg(all(
    feature = "storage-test-hooks",
    any(target_os = "linux", target_os = "macos")
))]
#[test]
fn injected_emfile_fails_only_the_kth_pipe_and_scope_restores_creation() {
    run_test(async {
        use cpforge::supervisor::PipeTestGuard;
        #[cfg(target_os = "linux")]
        let directory = "/proc/self/fd";
        #[cfg(target_os = "macos")]
        let directory = "/dev/fd";
        let count = || std::fs::read_dir(directory).unwrap().count();
        // Register the reactor before counting, then fail every partial prefix.
        drop(stdout_pipe().unwrap());
        let before = count();
        for k in 1..=3 {
            {
                let _failure = PipeTestGuard::fail_at(k);
                let mut pipes = Vec::new();
                for _ in 1..k {
                    pipes.push(stdout_pipe().unwrap());
                }
                assert_eq!(
                    stdout_pipe().unwrap_err().raw_os_error(),
                    Some(libc::EMFILE)
                );
                drop(pipes);
            }
            assert_eq!(count(), before);
            drop(stdout_pipe().unwrap());
            assert_eq!(count(), before);
        }
    });
}
