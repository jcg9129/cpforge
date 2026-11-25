use super::*;
use crate::config::{ConfigRequest, Environment, Platform, load_config};
use crate::names::Identifier;
use crate::storage::inputs::{CapturedSource, FrozenFile};
use crate::tests_store::{Origin, TestSettings, TestStore};
use std::fs;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!(
                "fingerprint-{}",
                crate::build::BuildId::generate().unwrap()
            ));
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("user.toml"), "schema = 1\n").unwrap();
        fs::write(path.join("cpforge.toml"), "schema = 1\n").unwrap();
        Self(crate::storage::paths::plain(
            &fs::canonicalize(path).unwrap(),
        ))
    }
    fn target(&self, extra: &str) -> TargetPlan {
        fs::write(self.0.join("problem.toml"), format!("schema = 1\n[problem]\nid = 'local:p'\n[config]\nlanguage = 'python'\nsources = ['a']\n{extra}")).unwrap();
        load_config(&ConfigRequest {
            environment: Environment::default(),
            platform: Platform::host(),
            current_dir: self.0.clone(),
            config_path: Some(self.0.join("user.toml")),
            workspace_root: Some(self.0.clone()),
            problem_file: Some(self.0.join("problem.toml")),
            ..ConfigRequest::default()
        })
        .target()
        .unwrap()
    }
    fn file(&self, name: &str, content: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, content).unwrap();
        path
    }
    fn source(&self) -> SourceFile {
        SourceFile {
            problem_path: "a".into(),
            content_path: self.file("a", b"X"),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn from_hex(text: &str) -> Vec<u8> {
    assert_eq!(text.len() % 2, 0);
    text.as_bytes()
        .chunks_exact(2)
        .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap())
        .collect()
}
#[test]
fn golden_source_bytes_and_sha256() {
    let f = Fixture::new();
    let target = f.target("");
    let files = [f.source()];
    let mut encoded = Vec::new();
    encode_source(&mut encoded, &files, &target).unwrap();
    // Domain, one (a, X) file, empty profile, one source a, no support,
    // no entry and build-free tag. All lengths are explicit u64 LE.
    let expected = from_hex(concat!(
        "10000000000000006370662d66702d736f757263652d7631",
        "0100000000000000010000000000000061010000000000000058",
        "0000000000000000",
        "0100000000000000010000000000000061",
        "0000000000000000",
        "00",
        "00"
    ));
    assert_eq!(encoded, expected);
    assert_eq!(
        source_fingerprint(&files, &target).unwrap().as_slice(),
        from_hex("e3dfd410146e370600880dd56f281ba6613b53d1f421d6be667b366e39d7f9c0")
    );
    assert_eq!(
        source_fingerprint(&files, &target).unwrap().as_slice(),
        &Sha256::digest(expected)[..]
    );
}
#[test]
fn golden_test_bytes_sort_ids_and_distinguish_absent_from_empty() {
    let f = Fixture::new();
    let make = |id: &str, content: &[u8], answer| FrozenTest {
        id: Identifier::new(id).unwrap(),
        origin: Origin::Custom,
        input: FrozenFile {
            path: f.file(id, content),
            len: content.len() as u64,
            sha256: Sha256::digest(content).into(),
        },
        answer,
        settings: TestSettings::default(),
    };
    let a = make("a", b"X", FrozenAnswer::Absent);
    let b = make(
        "b",
        b"",
        FrozenAnswer::Present {
            path: f.file("answer", b""),
            len: 0,
            sha256: Sha256::digest(b"").into(),
        },
    );
    let tests = [b, a];
    let mut encoded = Vec::new();
    encode_tests(&mut encoded, &tests).unwrap();
    let expected = from_hex(concat!(
        "0f000000000000006370662d66702d74657374732d7631",
        "020000000000000001000000000000006101000000000000005800",
        "0100000000000000620000000000000000010000000000000000"
    ));
    assert_eq!(encoded, expected);
    assert_eq!(
        tests_fingerprint(&tests).unwrap().as_slice(),
        from_hex("0d859e8e1cc88d4d122880e8650891c7217c736f46bd2cfd1f29ffceb344da00")
    );
    assert_eq!(
        tests_fingerprint(&tests).unwrap().as_slice(),
        &Sha256::digest(expected)[..]
    );
    let mut changed = tests.to_vec();
    changed[0].answer = FrozenAnswer::Absent;
    assert_ne!(
        tests_fingerprint(&tests).unwrap(),
        tests_fingerprint(&changed).unwrap()
    );
}
#[test]
fn golden_argv_build_settings() {
    let f = Fixture::new();
    let mut target = f.target("");
    // A build stage is obtained from validated C++ configuration; no expansion
    // or program resolution (and hence no compiler installation) is needed.
    fs::write(f.0.join("problem.toml"), "schema = 1\n[problem]\nid = 'local:p'\n[config]\nlanguage = 'cpp'\nentry = 'E'\n[config.build]\nargv = ['cc', '{sources}', '{entry}']\nenv_remove = ['Z', 'A']\nenv = { X = 'v', OFF = false }\n").unwrap();
    let built = load_config(&ConfigRequest {
        environment: Environment::default(),
        platform: Platform::host(),
        current_dir: f.0.clone(),
        config_path: Some(f.0.join("user.toml")),
        workspace_root: Some(f.0.clone()),
        problem_file: Some(f.0.join("problem.toml")),
        ..ConfigRequest::default()
    })
    .target()
    .unwrap();
    target.build = built.build;
    let mut encoded = Vec::new();
    encode_build(&mut encoded, target.build.as_ref().unwrap()).unwrap();
    let expected = from_hex(concat!(
        "00030000000000000002000000000000006363",
        "09000000000000007b736f75726365737d07000000000000007b656e7472797d",
        "0000020000000000000001000000000000004101000000000000005a",
        "020000000000000003000000000000004f46460001000000000000005801010000000000000076"
    ));
    assert_eq!(encoded, expected);
    let mut hash = HashWriter(Sha256::new());
    encode_build(&mut hash, target.build.as_ref().unwrap()).unwrap();
    assert_eq!(&hash.0.finalize()[..], &Sha256::digest(expected)[..]);
}
#[test]
fn source_roles_order_entry_profile_and_covered_build_fields_change_hash() {
    let f = Fixture::new();
    let mut target = f.target("support_files = ['b']\nentry = 'E'\n");
    let files = [
        f.source(),
        SourceFile {
            problem_path: "b".into(),
            content_path: f.file("b", b"X"),
        },
    ];
    let original = source_fingerprint(&files, &target).unwrap();
    let a = target.sources.pop().unwrap();
    let b = target.support_files.pop().unwrap();
    target.sources.push(b);
    target.support_files.push(a);
    assert_ne!(original, source_fingerprint(&files, &target).unwrap());
    let target = f.target("support_files = ['b']\nentry = 'Other'\n");
    assert_ne!(original, source_fingerprint(&files, &target).unwrap());
    let mut target = f.target("support_files = ['b']\nentry = 'E'\n");
    target.sources.append(&mut target.support_files);
    let ordered = source_fingerprint(&files, &target).unwrap();
    target.sources.reverse();
    assert_ne!(ordered, source_fingerprint(&files, &target).unwrap());
    target.profile = Some(target.language.clone());
    assert_ne!(ordered, source_fingerprint(&files, &target).unwrap());
    let mut reversed = files.to_vec();
    reversed.reverse();
    assert_eq!(
        source_fingerprint(&files, &target).unwrap(),
        source_fingerprint(&reversed, &target).unwrap()
    );
}
#[test]
fn captured_bytes_equal_original_recomputation_and_ignore_later_edits() {
    use crate::storage::{WorkspaceLock, admission::register_job, prune::PrunePolicy};
    let f = Fixture::new();
    let target = f.target("");
    let originals = [f.source()];
    f.file(
        ".cpforge/job-seq",
        b"schema = 1\nepoch = '0123456789abcdef0123456789abcdef'\nnext = '0'\n",
    );
    f.file("tests.toml", b"schema = 1\n");
    let lock = WorkspaceLock::new(&f.0).unwrap();
    let job = register_job(
        lock.acquire("fingerprint capture").unwrap(),
        PrunePolicy::new(20, 1 << 30),
        8 << 20,
        std::time::SystemTime::now(),
    )
    .unwrap();
    let frozen = job
        .inputs()
        .freeze(
            &TestStore::new(f.0.clone()),
            &[],
            &CapturedSource::from_target(&target),
            &f.0,
        )
        .unwrap();
    let copies: Vec<_> = frozen
        .sources()
        .files()
        .iter()
        .map(SourceFile::from)
        .collect();
    let hash = source_fingerprint(&copies, &target).unwrap();
    assert_eq!(hash, source_fingerprint(&originals, &target).unwrap());
    let second = register_job(
        lock.acquire("second fingerprint capture").unwrap(),
        PrunePolicy::new(20, 1 << 30),
        8 << 20,
        std::time::SystemTime::now(),
    )
    .unwrap();
    let frozen_second = second
        .inputs()
        .freeze(
            &TestStore::new(f.0.clone()),
            &[],
            &CapturedSource::from_target(&target),
            &f.0,
        )
        .unwrap();
    let other_copies: Vec<_> = frozen_second
        .sources()
        .files()
        .iter()
        .map(SourceFile::from)
        .collect();
    assert_ne!(copies[0].content_path, other_copies[0].content_path);
    assert_eq!(hash, source_fingerprint(&other_copies, &target).unwrap());
    f.file("a", b"changed");
    assert_eq!(hash, source_fingerprint(&copies, &target).unwrap());
    assert_ne!(hash, source_fingerprint(&originals, &target).unwrap());
}
#[cfg(unix)]
#[test]
fn golden_unix_non_utf8_path_bytes() {
    use std::os::unix::ffi::OsStrExt;
    let path = Path::new(OsStr::from_bytes(b"dir/\xff"));
    let mut encoded = Vec::new();
    bytes(&mut encoded, &path_bytes(path)).unwrap();
    let expected = b"\x05\0\0\0\0\0\0\0dir/\xff";
    assert_eq!(encoded, expected);
    let mut hash = HashWriter(Sha256::new());
    bytes(&mut hash, &path_bytes(path)).unwrap();
    let actual: [u8; 32] = hash.0.finalize().into();
    assert_eq!(actual.as_slice(), &Sha256::digest(expected)[..]);
    assert_eq!(
        actual.as_slice(),
        from_hex("d713922b3bb7febf9944f8ef8a7ae841e9a43107359a3f53e06206ec5d62384a")
    );
}
#[cfg(windows)]
#[test]
fn golden_windows_path_wtf8_and_forward_slash() {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    let native = OsString::from_wide(&[b'd' as u16, b'\\' as u16, 0xd800, 0xd83d, 0xde00]);
    let mut encoded = Vec::new();
    bytes(&mut encoded, &path_bytes(Path::new(&native))).unwrap();
    let expected = b"\x09\0\0\0\0\0\0\0d/\xed\xa0\x80\xf0\x9f\x98\x80";
    assert_eq!(encoded, expected);
    let mut hash = HashWriter(Sha256::new());
    bytes(&mut hash, &path_bytes(Path::new(&native))).unwrap();
    let actual: [u8; 32] = hash.0.finalize().into();
    assert_eq!(actual.as_slice(), &Sha256::digest(expected)[..]);
    assert_eq!(
        actual.as_slice(),
        from_hex("0013c7ee0766be63dd656b0317d8131f892441fea6e452909fd4bcac30b3ed12")
    );
}

fn built_target(f: &Fixture, settings: &str) -> TargetPlan {
    f.file("problem.toml", format!("schema = 1\n[problem]\nid = 'local:p'\n[config]\nlanguage = 'cpp'\nsources = ['a']\nentry = 'E'\n[config.build]\n{settings}\n").as_bytes());
    load_config(&ConfigRequest {
        environment: Environment::default(),
        platform: Platform::host(),
        current_dir: f.0.clone(),
        config_path: Some(f.0.join("user.toml")),
        workspace_root: Some(f.0.clone()),
        problem_file: Some(f.0.join("problem.toml")),
        ..ConfigRequest::default()
    })
    .target()
    .unwrap()
}
#[test]
fn golden_shell_settings_and_each_shell_field_affects_fingerprint() {
    let f = Fixture::new();
    let settings = "shell = { interpreter = ['sh', '-c'], script = 'echo {}', args = ['{entry}', '{{literal}}'] }\ncwd = { selector = 'problem' }";
    let target = built_target(&f, settings);
    let mut encoded = Vec::new();
    encode_build(&mut encoded, target.build.as_ref().unwrap()).unwrap();
    let expected = from_hex(concat!(
        "0102000000000000000200000000000000736802000000000000002d63",
        "07000000000000006563686f207b7d0200000000000000",
        "07000000000000007b656e7472797d0b000000000000007b7b6c69746572616c7d7d",
        "000100000000000000000000000000000000"
    ));
    assert_eq!(encoded, expected);
    let mut hash = HashWriter(Sha256::new());
    encode_build(&mut hash, target.build.as_ref().unwrap()).unwrap();
    assert_eq!(&hash.0.finalize()[..], &Sha256::digest(expected)[..]);
    let files = [f.source()];
    let original = source_fingerprint(&files, &target).unwrap();
    for changed in [
        settings.replace("'sh'", "'bash'"),
        settings.replace("echo {}", "echo changed"),
        settings.replace("'{{literal}}'", "'other'"),
        settings.replace("'problem'", "'workspace'"),
    ] {
        let target = built_target(&f, &changed);
        assert_ne!(original, source_fingerprint(&files, &target).unwrap());
    }
    let mut excluded = target.clone();
    let stage = excluded.build.as_mut().unwrap();
    stage.deadline.milliseconds.value += 1;
    stage.stdout.bytes.value += 1;
    stage.stderr.bytes.value += 1;
    stage.limits = crate::config::Limits::default();
    assert_eq!(original, source_fingerprint(&files, &excluded).unwrap());
}
#[test]
fn cwd_path_is_lexically_joined_not_canonicalized() {
    let f = Fixture::new();
    let target = built_target(&f, "argv = ['cc']\ncwd = { path = 'uncreated/../work' }");
    let mut encoded = Vec::new();
    encode_build(&mut encoded, target.build.as_ref().unwrap()).unwrap();
    // Joined as the loader joins it: the configured spelling is kept, so on
    // Windows its `/` separators stay as written.
    let path = f.0.join("uncreated/../work");
    let path = native_bytes(path.as_os_str());
    let mut expected = from_hex("0001000000000000000200000000000000636301");
    expected.extend_from_slice(&(path.len() as u64).to_le_bytes());
    expected.extend_from_slice(&path);
    expected.extend_from_slice(&[0; 16]); // env_remove and env counts
    assert_eq!(encoded, expected);
}

#[test]
fn golden_role_and_entry_vectors_change_hash_without_file_byte_changes() {
    let f = Fixture::new();
    let files = [f.source()];
    let mut support = f.target("");
    support.support_files = std::mem::take(&mut support.sources);
    let entry = f.target("entry = 'E'\n");
    // Domain and file, empty profile, sources, support files, entry, build tag.
    let role_bytes = from_hex(concat!(
        "10000000000000006370662d66702d736f757263652d7631",
        "0100000000000000010000000000000061010000000000000058",
        "0000000000000000",
        "0000000000000000",
        "0100000000000000010000000000000061",
        "00",
        "00"
    ));
    let entry_bytes = from_hex(concat!(
        "10000000000000006370662d66702d736f757263652d7631",
        "0100000000000000010000000000000061010000000000000058",
        "0000000000000000",
        "0100000000000000010000000000000061",
        "0000000000000000",
        "01010000000000000045",
        "00"
    ));
    let original = source_fingerprint(&files, &f.target("")).unwrap();
    for (target, expected) in [(support, role_bytes), (entry, entry_bytes)] {
        let mut encoded = Vec::new();
        encode_source(&mut encoded, &files, &target).unwrap();
        assert_eq!(encoded, expected);
        let hash = source_fingerprint(&files, &target).unwrap();
        assert_eq!(hash.as_slice(), &Sha256::digest(expected)[..]);
        assert_ne!(hash, original);
    }
}

fn judge_config() -> crate::report::CheckConfig {
    crate::report::CheckConfig {
        kind: crate::report::CheckMode::Checker,
        ..Default::default()
    }
}
fn judge_role(f: &Fixture) -> JudgeFingerprintRole {
    let run = f
        .target("entry = 'E'\n[config.run]\nargv = ['run', '{entry}']")
        .run;
    let mut build = run.clone();
    if let StageCommand::Argv(argv) = &mut build.command {
        argv[0].unexpanded = "cc".into();
        argv[1].unexpanded = "{sources}".into();
    }
    JudgeFingerprintRole {
        profile: crate::report::JudgeProfile::Testlib,
        files: vec![
            JudgeFingerprintFile {
                path: "z".into(),
                copied_bytes: vec![0, 255],
            },
            JudgeFingerprintFile {
                path: "a".into(),
                copied_bytes: b"X".to_vec(),
            },
        ],
        origin: JudgeFingerprintOrigin::Built(Box::new(JudgeFingerprintBuilt {
            sources: vec!["a".into()],
            support_files: vec![],
            assets: vec!["z".into()],
            language: "python".into(),
            entry: Some("E".into()),
            build: Some(build),
            run,
        })),
        deadline_us: Some(1_234_000),
        stdout_cap: Some(10),
        stderr_cap: None,
        seed: crate::report::SeedMode::None,
    }
}
fn judge_built(role: &mut JudgeFingerprintRole) -> &mut JudgeFingerprintBuilt {
    let JudgeFingerprintOrigin::Built(built) = &mut role.origin else {
        panic!("built role")
    };
    built
}
fn judge_bytes(
    check: &crate::report::CheckConfig,
    roles: &[Option<JudgeFingerprintRole>; 3],
) -> Vec<u8> {
    let mut bytes = Vec::new();
    encode_judge(&mut bytes, check, roles).unwrap();
    bytes
}
#[test]
fn judge_independent_bytes_and_hash_vectors() {
    use crate::report as r;
    let f = Fixture::new();
    let built = judge_role(&f);
    let mut prebuilt = built.clone();
    prebuilt.origin = JudgeFingerprintOrigin::Prebuilt {
        program: "a".into(),
        assets: vec!["z".into()],
    };
    prebuilt.profile = crate::report::JudgeProfile::Simple;
    let mut shell_role = built.clone();
    let shell = built_target(&f, "shell = { interpreter = ['sh', '-c'], script = 'echo {}', args = ['{entry}', 'x'] }\ncwd = { selector = 'run' }\nenv_remove = ['Z', 'A']\nenv = { X = 'v', OFF = false }").build.unwrap();
    let b = judge_built(&mut shell_role);
    b.build = Some(shell.clone());
    b.run = shell;
    shell_role.deadline_us = None;
    shell_role.stdout_cap = None;
    shell_role.stderr_cap = Some(12);
    shell_role.seed = r::SeedMode::Argv;
    let interactive = r::CheckConfig {
        kind: r::CheckMode::Interactive,
        answer: r::AnswerPolicy::None,
        scoring: r::Scoring::Points {
            scale: r::Score::parse("100").unwrap(),
            aggregation: r::Aggregation::Min,
        },
        display: r::DisplayProfile::JudgeCf,
        interactive: Some(r::InteractiveConfig {
            checking: r::InteractiveChecking::InteractorChecker,
            mode: r::InteractiveMode::Relay,
            idle_us: Some(1_000_000),
            early_reader_close: true,
            interactor_allowance_us: 2_000_000,
            peer_allowance_us: 3_000_000,
            solution_seed: r::SeedMode::Env,
            interactor_seed: r::SeedMode::Argv,
        }),
    };
    let vectors: serde_json::Value = serde_json::from_slice(include_bytes!(
        "../../../report/wire/fixtures/judge_fingerprints.json"
    ))
    .unwrap();
    let cases = [
        (judge_config(), [None, None, None]),
        (judge_config(), [Some(built.clone()), None, None]),
        (judge_config(), [Some(prebuilt.clone()), None, None]),
        (
            judge_config(),
            [Some(prebuilt), Some(built.clone()), Some(built)],
        ),
        (interactive, [None, None, Some(shell_role)]),
    ];
    assert_eq!(cases.len(), vectors.as_array().unwrap().len());
    for ((check, roles), vector) in cases.iter().zip(vectors.as_array().unwrap()) {
        let bytes = judge_bytes(check, roles);
        assert_eq!(bytes, from_hex(vector["input_hex"].as_str().unwrap()));
        assert_eq!(
            &Sha256::digest(&bytes)[..],
            from_hex(vector["sha256"].as_str().unwrap())
        );
        let hash = judge_fingerprint(check, roles).unwrap();
        if roles.iter().all(Option::is_none) {
            assert_eq!(hash, None);
        } else {
            assert_eq!(hash.unwrap().as_slice(), &Sha256::digest(&bytes)[..]);
        }
    }
}
#[test]
fn judge_role_presence_profiles_files_lists_and_overrides_are_covered() {
    let f = Fixture::new();
    let role = judge_role(&f);
    let original = judge_fingerprint(&judge_config(), &[Some(role.clone()), None, None]).unwrap();
    type Change = fn(&mut JudgeFingerprintRole);
    let changes: &[Change] = &[
        |r| r.profile = crate::report::JudgeProfile::Simple,
        |r| r.files[0].path = "y".into(),
        |r| r.files[0].copied_bytes[0] = 1,
        |r| {
            r.files.pop();
        },
        |r| judge_built(r).sources.push("b".into()),
        |r| judge_built(r).sources.insert(0, "b".into()),
        |r| {
            let b = judge_built(r);
            b.support_files.append(&mut b.sources);
        },
        |r| judge_built(r).support_files.push("b".into()),
        |r| judge_built(r).assets.push("b".into()),
        |r| judge_built(r).assets.insert(0, "b".into()),
        |r| judge_built(r).language = "cpp".into(),
        |r| judge_built(r).entry = None,
        |r| judge_built(r).entry = Some("F".into()),
        |r| judge_built(r).build = None,
        |r| r.deadline_us = None,
        |r| r.deadline_us = Some(1_234_001),
        |r| r.stdout_cap = None,
        |r| r.stdout_cap = Some(11),
        |r| r.stderr_cap = Some(0),
        |r| r.stderr_cap = Some(1),
        |r| r.seed = crate::report::SeedMode::Env,
        |r| r.seed = crate::report::SeedMode::Argv,
        |r| {
            r.origin = JudgeFingerprintOrigin::Prebuilt {
                program: "a".into(),
                assets: vec!["z".into()],
            }
        },
    ];
    for (index, change) in changes.iter().enumerate() {
        let mut changed = role.clone();
        change(&mut changed);
        assert_ne!(
            original,
            judge_fingerprint(&judge_config(), &[Some(changed), None, None]).unwrap(),
            "field {index}"
        );
    }
    for slot in [1, 2] {
        let mut roles = [None, None, None];
        roles[slot] = Some(role.clone());
        assert_ne!(
            original,
            judge_fingerprint(&judge_config(), &roles).unwrap()
        );
        roles[0] = Some(role.clone());
        assert_ne!(
            original,
            judge_fingerprint(&judge_config(), &roles).unwrap()
        );
    }
    let mut reordered = role.clone();
    reordered.files.reverse();
    assert_eq!(
        original,
        judge_fingerprint(&judge_config(), &[Some(reordered), None, None]).unwrap()
    );
    // Reordering any configured list is significant even with the same files.
    for list_index in 0..3 {
        let mut ordered = role.clone();
        let b = judge_built(&mut ordered);
        let list = match list_index {
            0 => &mut b.sources,
            1 => &mut b.support_files,
            _ => &mut b.assets,
        };
        *list = vec!["first".into(), "second".into()];
        let hash =
            judge_fingerprint(&judge_config(), &[Some(ordered.clone()), None, None]).unwrap();
        let b = judge_built(&mut ordered);
        match list_index {
            0 => b.sources.reverse(),
            1 => b.support_files.reverse(),
            _ => b.assets.reverse(),
        }
        assert_ne!(
            hash,
            judge_fingerprint(&judge_config(), &[Some(ordered), None, None]).unwrap()
        );
    }
    let mut prebuilt = role;
    prebuilt.origin = JudgeFingerprintOrigin::Prebuilt {
        program: "a".into(),
        assets: vec!["z".into(), "b".into()],
    };
    let original =
        judge_fingerprint(&judge_config(), &[Some(prebuilt.clone()), None, None]).unwrap();
    for program_change in [false, true] {
        let mut changed = prebuilt.clone();
        if let JudgeFingerprintOrigin::Prebuilt { program, assets } = &mut changed.origin {
            if program_change {
                *program = "b".into();
            } else {
                assets.reverse();
            }
        }
        assert_ne!(
            original,
            judge_fingerprint(&judge_config(), &[Some(changed), None, None]).unwrap()
        );
    }
}
fn stage_text(stage: &StagePlan, value: &str) -> crate::config::Sourced<String> {
    crate::config::Sourced {
        value: value.into(),
        origin: stage.cwd.origin.clone(),
        provenance: stage.cwd.provenance.clone(),
        overlay: stage.cwd.overlay.clone(),
    }
}

#[test]
fn judge_build_and_run_cover_all_source_stage_settings() {
    let f = Fixture::new();
    let role = judge_role(&f);
    let original = judge_fingerprint(&judge_config(), &[Some(role.clone()), None, None]).unwrap();
    type Change = fn(&mut StagePlan);
    let changes: &[Change] = &[
        |s| {
            if let StageCommand::Argv(a) = &mut s.command {
                a.reverse();
            }
        },
        |s| {
            if let StageCommand::Argv(a) = &mut s.command {
                a[0].unexpanded = "changed".into();
            }
        },
        |s| s.cwd.value = Cwd::Problem,
        |s| s.environment.push(EnvEdit::Remove(stage_text(s, "A"))),
        |s| s.environment.push(EnvEdit::Unset(stage_text(s, "A"))),
        |s| {
            s.environment.push(EnvEdit::Set {
                name: stage_text(s, "A"),
                value: stage_text(s, "v"),
            })
        },
    ];
    for build in [false, true] {
        for change in changes {
            let mut changed = role.clone();
            let built = judge_built(&mut changed);
            change(if build {
                built.build.as_mut().unwrap()
            } else {
                &mut built.run
            });
            assert_ne!(
                original,
                judge_fingerprint(&judge_config(), &[Some(changed), None, None]).unwrap()
            );
        }
        let shell = "shell = { interpreter = ['sh', '-c'], script = 'echo {}', args = ['{entry}', 'x'] }\ncwd = { selector = 'run' }\nenv_remove = ['Z', 'A']\nenv = { X = 'v', OFF = false }";
        let mut baseline = role.clone();
        let stage = built_target(&f, shell).build.unwrap();
        let built = judge_built(&mut baseline);
        *if build {
            built.build.as_mut().unwrap()
        } else {
            &mut built.run
        } = stage;
        let original =
            judge_fingerprint(&judge_config(), &[Some(baseline.clone()), None, None]).unwrap();
        for settings in [
            // The interpreter flag is fixed at `-c` by the configuration rules,
            // so only the program can vary.
            shell.replace("'sh'", "'bash'"),
            shell.replace("echo {}", "echo other"),
            shell.replace("'x'", "'y'"),
            shell.replace("'Z'", "'Y'"),
            shell.replace("'v'", "'w'"),
            shell.replace("false", "'set'"),
            shell.replace("X =", "Y ="),
        ] {
            let mut changed = baseline.clone();
            let built = judge_built(&mut changed);
            *if build {
                built.build.as_mut().unwrap()
            } else {
                &mut built.run
            } = built_target(&f, &settings).build.unwrap();
            assert_ne!(
                original,
                judge_fingerprint(&judge_config(), &[Some(changed), None, None]).unwrap()
            );
        }
        let mut unchanged = baseline;
        let built = judge_built(&mut unchanged);
        let stage = if build {
            built.build.as_mut().unwrap()
        } else {
            &mut built.run
        };
        stage.environment.reverse();
        stage.deadline.milliseconds.value += 1;
        stage.stdout.bytes.value += 1;
        stage.stderr.bytes.value += 1;
        assert_eq!(
            original,
            judge_fingerprint(&judge_config(), &[Some(unchanged), None, None]).unwrap()
        );
    }
}
#[test]
fn judge_check_configuration_is_covered_except_display() {
    use crate::report as r;
    let f = Fixture::new();
    let roles = [Some(judge_role(&f)), None, None];
    let check = judge_config();
    let hash = judge_fingerprint(&check, &roles).unwrap();
    let mut changed = check.clone();
    changed.display = r::DisplayProfile::JudgeCf;
    assert_eq!(hash, judge_fingerprint(&changed, &roles).unwrap());
    changed = check.clone();
    changed.answer = r::AnswerPolicy::None;
    assert_ne!(hash, judge_fingerprint(&changed, &roles).unwrap());
    changed = check.clone();
    changed.kind = r::CheckMode::Compare;
    assert_ne!(hash, judge_fingerprint(&changed, &roles).unwrap());
    let mut point_hashes = std::collections::BTreeSet::new();
    for aggregation in [r::Aggregation::Sum, r::Aggregation::Min] {
        for scale in ["1", "2"] {
            changed = check.clone();
            changed.scoring = r::Scoring::Points {
                scale: r::Score::parse(scale).unwrap(),
                aggregation,
            };
            assert_ne!(hash, judge_fingerprint(&changed, &roles).unwrap());
            assert!(point_hashes.insert(judge_fingerprint(&changed, &roles).unwrap()));
        }
    }
    let mut check = check;
    check.kind = r::CheckMode::Interactive;
    check.interactive = Some(r::InteractiveConfig {
        checking: r::InteractiveChecking::Interactor,
        mode: r::InteractiveMode::Relay,
        idle_us: Some(10),
        early_reader_close: false,
        interactor_allowance_us: 20,
        peer_allowance_us: 30,
        solution_seed: r::SeedMode::None,
        interactor_seed: r::SeedMode::None,
    });
    assert_ne!(hash, judge_fingerprint(&check, &roles).unwrap());
    let hash = judge_fingerprint(&check, &roles).unwrap();
    type Change = fn(&mut r::InteractiveConfig);
    let changes: &[Change] = &[
        |i| i.checking = r::InteractiveChecking::Checker,
        |i| i.checking = r::InteractiveChecking::InteractorChecker,
        |i| i.mode = r::InteractiveMode::Direct,
        |i| i.idle_us = None,
        |i| i.idle_us = Some(11),
        |i| i.early_reader_close = true,
        |i| i.interactor_allowance_us += 1,
        |i| i.peer_allowance_us += 1,
        |i| i.solution_seed = r::SeedMode::Env,
        |i| i.interactor_seed = r::SeedMode::Argv,
    ];
    for change in changes {
        let mut changed = check.clone();
        change(changed.interactive.as_mut().unwrap());
        assert_ne!(hash, judge_fingerprint(&changed, &roles).unwrap());
    }
}
