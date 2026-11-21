use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use cpforge::build::BuildId;
use cpforge::config::{ConfigRequest, Environment, Platform, TargetPlan, load_config};
use cpforge::exec::{Baseline, Executable, Probe, ProbeResult, ProblemBinding};
use cpforge::jobs::PrepareJobInputs;
use cpforge::storage::WorkspaceLock;
use cpforge::storage::admission::{RegisteredJob, register_job};
use cpforge::storage::job::{ProbeOutcome, probe};
use cpforge::storage::prune::{PrunePolicy, reclaim, select_and_rename};
use cpforge::tests_store::TestStore;

#[derive(Debug)]
pub struct Fixture(pub PathBuf);
impl Fixture {
    pub fn new() -> Self {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("ready-job-{}", BuildId::generate().unwrap()));
        fs::create_dir_all(&path).unwrap();
        let f = Self(cpforge::storage::paths::plain(
            &fs::canonicalize(path).unwrap(),
        ));
        f.write("user.toml", "schema = 1\n");
        f.write(
            "cpforge.toml",
            "schema = 1\n[build]\ndeadline_s = 60\n[run]\ndeadline_s = 10\n",
        );
        f.write(
            ".cpforge/job-seq",
            "schema = 1\nepoch = '0123456789abcdef0123456789abcdef'\nnext = '0'\n",
        );
        // Homebrew GCC on macOS (`g++` is Apple clang there). The user layer is
        // used so that a test's own user.toml, or any workspace-layer language
        // table, still replaces it.
        #[cfg(target_os = "macos")]
        f.write(
            "user.toml",
            "schema = 1\n[languages.cpp.build]\nargv = ['g++-14', '-std=gnu++23', '-O2', '{sources}', '-o', '{artifact}']\n",
        );
        fs::create_dir_all(f.path("problem/tests")).unwrap();
        f
    }
    pub fn path(&self, relative: &str) -> PathBuf {
        relative
            .split('/')
            .fold(self.0.clone(), |path, part| path.join(part))
    }
    pub fn write(&self, relative: &str, text: &str) {
        let path = self.path(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    pub fn target(&self, language: &str, extra: &str) -> TargetPlan {
        self.write(
            "problem/problem.toml",
            &format!(
                "schema = 1\n[problem]\nid = 'local:p'\n[config]\nlanguage = '{language}'\n{extra}"
            ),
        );
        load_config(&ConfigRequest {
            environment: Environment::default(),
            platform: Platform::host(),
            current_dir: self.0.clone(),
            config_path: Some(self.path("user.toml")),
            workspace_root: Some(self.0.clone()),
            problem_file: Some(self.path("problem/problem.toml")),
            ..ConfigRequest::default()
        })
        .target()
        .unwrap()
    }
    pub fn job(&self, limit: u64, policy: PrunePolicy) -> RegisteredJob {
        let lock = WorkspaceLock::new(&self.0).unwrap();
        register_job(
            lock.acquire("ready job fixture").unwrap(),
            policy,
            limit,
            SystemTime::now(),
        )
        .unwrap()
    }
    pub fn store(&self) -> TestStore {
        TestStore::new(self.path("problem"))
    }
    pub fn inputs<'a>(&self, probe: &'a dyn Probe) -> PrepareJobInputs<'a> {
        PrepareJobInputs {
            workspace_root: self.0.clone(),
            problem: ProblemBinding {
                dir: self.path("problem"),
                id: "local:p".into(),
            },
            report: cpforge::jobs::engine::ReportPresentation {
                problem_name: "problem".into(),
                problem_kind: "batch".into(),
                target_name: None,
                published_limits: None,
            },
            baseline: Baseline::from_entries(vec![
                ("PATH".into(), std::env::var_os("PATH").unwrap_or_default()),
                ("LANG".into(), "C.UTF-8".into()),
                ("PATHEXT".into(), ".EXE".into()),
            ]),
            platform: Platform::host(),
            build_bytes: 64 << 20,
            probe,
        }
    }
    pub fn prune(&self) {
        let lock = WorkspaceLock::new(&self.0).unwrap();
        let guard = lock.acquire("ready job prune").unwrap();
        let selection = select_and_rename(&guard, PrunePolicy::zero());
        assert!(selection.report.errors.is_empty(), "{selection:?}");
        assert!(selection.report.failed_renames.is_empty(), "{selection:?}");
        drop(guard);
        let report = reclaim(selection.runs_dir);
        assert!(report.errors.is_empty(), "{report:?}");
        assert!(report.walk.errors.is_empty(), "{report:?}");
        assert!(report.walk.skipped.is_empty(), "{report:?}");
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Debug)]
pub struct Tools;
impl Probe for Tools {
    fn probe(&self, _: &Path) -> std::io::Result<ProbeResult> {
        Ok(ProbeResult::Regular(Executable::Yes))
    }
}

/// A released lease can stay `Held` briefly: on Unix a concurrent spawn shares
/// open file descriptions between fork and exec, and Windows releases a
/// terminated process's locks asynchronously. Only `Held` is retried, within a
/// bound; every other outcome except `Acquired` fails at once.
pub fn released(job_dir: &Path) {
    let start = Instant::now();
    loop {
        match probe(job_dir) {
            ProbeOutcome::Acquired(lock) => {
                drop(lock);
                return;
            }
            ProbeOutcome::Held if start.elapsed() < Duration::from_secs(10) => {
                std::thread::sleep(Duration::from_millis(10));
            }
            outcome => panic!("expected an inactive lease, got {outcome:?}"),
        }
    }
}
