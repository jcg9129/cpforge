//! Judge facts and read-only status, including independent byte vectors.
use cpforge::jobs::CaptureCheck;
use cpforge::report::JobVerdict;
use cpforge::storage::Record;
use cpforge::storage::facts::{FactRecord, read_fact};
use cpforge::storage::job::{Epoch, JobId};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const CHECK: &str = "[check]\nkind = 'checker'\nchecker = { sources = ['checker.sh'], language = 'script', profile = 'simple' }\nscoring = { points = true, scale = 100 }\n";
const FIXED_ID: &str = "20260101000000000-0123456789abcdef";

#[cfg(target_os = "linux")]
const GOLDEN_FACT: &[u8] = include_bytes!("fixtures/judge_facts/linux.toml");
#[cfg(target_os = "linux")]
const GOLDEN_STATUS: &[u8] = include_bytes!("fixtures/judge_facts/linux.status.json");
#[cfg(target_os = "macos")]
const GOLDEN_FACT: &[u8] = include_bytes!("fixtures/judge_facts/macos.toml");
#[cfg(target_os = "macos")]
const GOLDEN_STATUS: &[u8] = include_bytes!("fixtures/judge_facts/macos.status.json");
#[cfg(windows)]
const GOLDEN_FACT: &[u8] = include_bytes!("fixtures/judge_facts/windows.toml");
#[cfg(windows)]
const GOLDEN_STATUS: &[u8] = include_bytes!("fixtures/judge_facts/windows.status.json");

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!(
                "judge-facts-{}",
                cpforge::build::BuildId::generate().unwrap()
            ));
        fs::create_dir_all(&root).unwrap();
        let f = Self(cpforge::storage::paths::plain(
            &fs::canonicalize(root).unwrap(),
        ));
        f.write("cpforge.toml", "schema = 1\n[languages.script]\nextensions = ['sh']\nsources = ['main.sh']\nbuild = false\n[languages.script.run]\nargv = ['sh', '{source}']\n");
        f.write("user/cpforge/config.toml", "schema = 1\n");
        f.write(
            ".cpforge/job-seq",
            "schema = 1\nepoch = '0123456789abcdef0123456789abcdef'\nnext = '0'\n",
        );
        f.write("problems/A/main.sh", "printf answer\n");
        f.write("problems/A/checker.sh", "exit 0\n");
        f.write("problems/A/validator.sh", "exit 0\n");
        f.write("problems/A/tests.toml", "schema = 1\n[[test]]\nid = 'a'\ninput = 'tests/a.in'\nanswer = 'tests/a.ans'\norigin = 'custom'\nenabled = true\n");
        f.write("problems/A/tests/a.in", "input\n");
        f.write("problems/A/tests/a.ans", "answer\n");
        f.problem(CHECK);
        f
    }
    fn path(&self, path: &str) -> PathBuf {
        path.split('/')
            .fold(self.0.clone(), |base, part| base.join(part))
    }
    fn write(&self, path: &str, bytes: impl AsRef<[u8]>) {
        let path = self.path(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    fn problem(&self, check: &str) {
        self.write("problems/A/problem.toml", format!("schema = 1\n[problem]\nid = 'local:A'\n[config]\nlanguage = 'script'\n[files]\nuser = ['checker.sh', 'validator.sh']\n{check}"));
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cpf"));
        command
            .current_dir(&self.0)
            .env("HOME", self.path("user"))
            .env("USERPROFILE", self.path("user"))
            .env("XDG_CONFIG_HOME", self.path("user"))
            .env("APPDATA", self.path("user"))
            .env_remove("CPF_STORAGE_CRASH_AT")
            .env_remove("CPF_STORAGE_PAUSE_AT")
            .env_remove("CPF_STORAGE_FIXED_JOB_ID")
            .env_remove("CPF_STORAGE_FIXED_FACT_TIME_MS")
            .env_remove("CPF_STORAGE_FACT_TEST_HOOK")
            .env_remove("CPF_STORAGE_FACT_RECOVERY_REMOVE_FAILURE")
            .env_remove("CPF_HOOK_DEPTH")
            .stdin(Stdio::null())
            .args(["--problem", "A"])
            .args(args);
        command
    }
    fn ok(&self, args: &[&str]) -> Output {
        let output = self.command(args).output().unwrap();
        assert_eq!(output.status.code(), Some(0), "{args:?}: {output:?}");
        output
    }
    fn fact(&self) -> FactRecord {
        match read_fact(self.path(".cpforge/facts/A/solution.toml")) {
            Record::Valid(fact) => fact,
            other => panic!("expected valid fact: {other:?}"),
        }
    }
    fn status(&self, judge: &str, comparison: &str) -> Value {
        let before = fs::read(self.path(".cpforge/facts/A/solution.toml")).unwrap();
        let sequence = fs::read(self.path(".cpforge/job-seq")).unwrap();
        let output = self.ok(&["status", "--json"]);
        let document: Value = serde_json::from_slice(&output.stdout).unwrap();
        let target = &document["problems"][0]["targets"][0];
        assert_eq!(target["judge"], judge, "{document}");
        assert_eq!(target["comparison"], comparison, "{document}");
        let human = self.ok(&["status"]);
        let human = String::from_utf8(human.stdout).unwrap();
        if judge == "changed" {
            assert!(human.contains("judge changed"), "{human}");
            assert!(!human.contains("solution: unchanged"), "{human}");
        } else if comparison == "unchanged" && matches!(judge, "none" | "unchanged") {
            assert!(human.contains("solution: unchanged"), "{human}");
        }
        assert_eq!(
            fs::read(self.path(".cpforge/facts/A/solution.toml")).unwrap(),
            before
        );
        assert_eq!(fs::read(self.path(".cpforge/job-seq")).unwrap(), sequence);
        document
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn judged_job(&self) -> Value {
        let output = self.ok(&["test", "--json"]);
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        let fact = self.fact();
        assert_eq!(
            fact.judge.map(|hash| hex(&hash)),
            report["fingerprints"]["judge"].as_str().map(str::to_owned)
        );
        assert!(fact.judge.is_some());
        assert_eq!(report["fact"]["outcome"]["kind"], "published", "{report}");
        report
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn digest(text: &str) -> [u8; 32] {
    std::array::from_fn(|i| u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).unwrap())
}
// The digests are derived by hand from the contract encodings of this
// fixture (workspace design §3.2 `source` and `tests`, configuration contract
// §9.1.3 judge) and are platform-independent: '/'-joined paths, UTF-8 native
// strings, no line-ending conversion. On a mismatch the fixed real job prints
// the actual fact bytes, which carry the digests the encoders produced.
fn fixed_fact() -> FactRecord {
    FactRecord {
        epoch: Epoch::parse("0123456789abcdef0123456789abcdef").unwrap(),
        n: 0,
        job: JobId::parse(FIXED_ID).unwrap(),
        finished_at: "2026-01-01T00:00:00.000Z".into(),
        verdict: JobVerdict::Scored,
        source: digest("dda68485ef21c922f97824c73db1de375ecdca1b06fac8037083126e7846deee"),
        tests: digest("cdec1c4525aea3048b60abe53bb7de69ed0a2e122a7f3c24c90053ff8d8d1587"),
        judge: Some(digest(
            "5e996bb36d2abaa6678e56e54679dd5abfcc9300e986a9a20ffb1a9192a458ab",
        )),
        capture_check: CaptureCheck::Checked,
    }
}
fn bytes_equal(actual: &[u8], expected: &[u8]) {
    assert_eq!(
        actual,
        expected,
        "actual bytes:\n{}",
        String::from_utf8_lossy(actual)
    );
}

#[test]
fn constructed_fact_and_current_status_match_committed_os_vectors() {
    let f = Fixture::new();
    let fact = fixed_fact();
    bytes_equal(&fact.encode(), GOLDEN_FACT);
    f.write(".cpforge/facts/A/solution.toml", fact.encode());
    assert_eq!(f.fact(), fact);
    let output = f.ok(&["status", "--json"]);
    bytes_equal(&output.stdout, GOLDEN_STATUS);
    f.status("unchanged", "unchanged");
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn real_simple_judge_fact_tracks_bytes_scoring_display_and_role_transitions() {
    let f = Fixture::new();
    let report = f.judged_job();
    assert_eq!(report["result"]["verdict"], "scored");
    f.status("unchanged", "unchanged");
    f.write("problems/A/checker.sh", "exit 1\n"); // Exactly one changed byte.
    f.status("changed", "unchanged");
    f.write("problems/A/checker.sh", "exit 0\n");
    f.problem(&CHECK.replace("scale = 100", "scale = 101"));
    f.status("changed", "unchanged");
    f.problem(&format!("{CHECK}display = 'judge-cf'\n"));
    f.status("unchanged", "unchanged");
    f.problem("");
    f.status("changed", "unchanged");
    f.ok(&["test", "--json"]);
    assert!(f.fact().judge.is_none());
    let bytes = fs::read_to_string(f.path(".cpforge/facts/A/solution.toml")).unwrap();
    assert!(!bytes.contains("judge ="));
    f.status("none", "unchanged");
    f.problem(CHECK);
    f.status("changed", "unchanged");
    f.write(".cpforge/facts/A/solution.toml", "schema = 1\njudge = 42\n");
    let status = f.status("unknown", "unknown");
    assert_eq!(
        status["problems"][0]["targets"][0]["fact_state"],
        "malformed"
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn validator_changes_and_unreadable_role_have_independent_judge_status() {
    let f = Fixture::new();
    f.problem(&format!("{CHECK}validator = {{ sources = ['validator.sh'], language = 'script', profile = 'simple' }}\n"));
    f.judged_job();
    f.status("unchanged", "unchanged");
    f.write("problems/A/validator.sh", "exit 1\n");
    f.status("changed", "unchanged");
    fs::remove_file(f.path("problems/A/checker.sh")).unwrap();
    let status = f.status("unknown", "unchanged");
    assert!(
        status["problems"][0]["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d
                .as_str()
                .unwrap()
                .contains("current judge not fingerprintable")),
        "{status}"
    );
}

#[cfg(all(
    feature = "storage-test-hooks",
    any(target_os = "linux", target_os = "macos")
))]
#[test]
fn fixed_real_job_and_status_match_committed_os_vectors() {
    let f = Fixture::new();
    let output = f
        .command(&["test", "--json"])
        .env("CPF_STORAGE_FIXED_JOB_ID", FIXED_ID)
        .env("CPF_STORAGE_FIXED_FACT_TIME_MS", "1767225600000")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report["fingerprints"]["judge"],
        hex(&fixed_fact().judge.unwrap())
    );
    bytes_equal(
        &fs::read(f.path(".cpforge/facts/A/solution.toml")).unwrap(),
        GOLDEN_FACT,
    );
    bytes_equal(&f.ok(&["status", "--json"]).stdout, GOLDEN_STATUS);
}

#[cfg(all(
    feature = "storage-test-hooks",
    any(target_os = "linux", target_os = "macos")
))]
#[test]
fn real_cpf_death_at_fact_replace_preserves_old_or_complete_new_judge() {
    // Same crash-child protocol as the storage fact commit test, now driving cpf.
    // Admission replaces job-seq first, so the fact rename is occurrence two.
    for (point, committed) in [
        ("fact.before_replace", false),
        ("atomic_replace.temp_synced#2", false),
        ("atomic_replace.renamed#2", true),
        ("fact.after_replace", true),
    ] {
        let f = Fixture::new();
        f.judged_job();
        let old = fs::read(f.path(".cpforge/facts/A/solution.toml")).unwrap();
        let old_judge = f.fact().judge;
        f.write("problems/A/checker.sh", "exit 0\n# new judge\n");
        let mut command = f.command(&["test", "--json"]);
        // Match the fact crash-child's process-local core-dump suppression.
        use std::os::unix::process::CommandExt;
        // SAFETY: only setrlimit, with a valid pointer, runs before exec.
        unsafe {
            command.pre_exec(|| {
                let limit = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                if libc::setrlimit(libc::RLIMIT_CORE, &limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let output = command
            .env("CPF_STORAGE_CRASH_AT", point)
            .env("CPF_STORAGE_FIXED_JOB_ID", FIXED_ID)
            .env("CPF_STORAGE_FIXED_FACT_TIME_MS", "1767225600000")
            .output()
            .unwrap();
        assert!(!output.status.success(), "{point}: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(&format!(
                "CPF-CRASH-AT {}",
                point.split('#').next().unwrap()
            )),
            "{output:?}"
        );
        if committed {
            let new = f.fact();
            assert_eq!(new.job.as_str(), FIXED_ID);
            assert_eq!(new.n, 1);
            assert!(new.judge.is_some());
            assert_ne!(new.judge, old_judge);
            assert_eq!(
                new.encode(),
                fs::read(f.path(".cpforge/facts/A/solution.toml")).unwrap()
            );
            f.status("unchanged", "unchanged");
        } else {
            bytes_equal(
                &fs::read(f.path(".cpforge/facts/A/solution.toml")).unwrap(),
                &old,
            );
            f.status("changed", "unchanged");
        }
    }
}
