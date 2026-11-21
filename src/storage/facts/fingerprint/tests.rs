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
    let path = f.0.join("uncreated").join("..").join("work");
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
