//! Store API invariants, including deterministic concurrent editor replay.

mod storage_support;

use std::fs;

use cpforge::storage::{Record, WorkspaceLock};
use cpforge::tests_store::{Answer, CapturedFile, Health, Origin, RegisterRequest, TestStore};
use storage_support::{Directory, assert_no_temps};

fn setup(directory: &Directory) -> TestStore {
    fs::create_dir_all(directory.path("problems/A/tests")).unwrap();
    TestStore::new(directory.path("problems/A"))
}

fn request(directory: &Directory, input: &str) -> RegisterRequest {
    RegisterRequest {
        current_dir: directory.path("problems/A"),
        input: input.into(),
        answer: None,
        no_answer: true,
        id: None,
    }
}

fn entry(id: &str, input: &str) -> String {
    format!("\n[[test]]\nid = '{id}'\ninput = 'tests/{input}'\norigin = 'custom'\nenabled = true\n")
}

#[test]
fn list_reserves_both_roles_and_reports_health_without_catalog_failure() {
    let directory = Directory::new();
    let store = setup(&directory);
    let text = entry("one", "one.in")
        + &entry("two", "two.in").replace("origin =", "answer = 'tests/reserved.in'\norigin =")
        + &entry("three", "three.in").replace("origin =", "answer = 'tests/shared.ans'\norigin =");
    fs::write(directory.path("problems/A/tests.toml"), text).unwrap();
    fs::write(directory.path("problems/A/tests/reserved.in"), b"data").unwrap();
    fs::create_dir(directory.path("problems/A/tests/two.in")).unwrap();
    fs::write(directory.path("problems/A/tests/free.in"), b"free").unwrap();
    fs::write(directory.path("problems/A/tests/free.ans"), b"").unwrap();
    // A sibling registered by another entry is not offered with the candidate.
    fs::write(directory.path("problems/A/tests/shared.in"), b"data").unwrap();
    fs::write(directory.path("problems/A/tests/shared.ans"), b"data").unwrap();
    let listing = store.list().unwrap();
    assert!(matches!(listing.catalog, Record::Valid(_)));
    assert!(matches!(listing.registered[0].input, Health::Missing));
    assert!(matches!(
        listing.registered[1].input,
        Health::NotRegular { kind: "directory" }
    ));
    assert_eq!(listing.candidates.len(), 2);
    assert_eq!(listing.candidates[0].input, "tests/free.in");
    assert_eq!(
        listing.candidates[0].answer,
        Answer::Named("tests/free.ans".into())
    );
    assert_eq!(listing.candidates[1].input, "tests/shared.in");
    assert_eq!(listing.candidates[1].answer, Answer::Absent);
    assert!(listing.strays.is_empty());
    assert!(!directory.path(".cpforge/lock").exists());
    assert_no_temps(&directory.path("problems/A"));
}

#[test]
fn register_refuses_unhealthy_explicit_and_inferred_answers() {
    let directory = Directory::new();
    let store = setup(&directory);
    fs::write(directory.path("problems/A/tests/one.in"), b"data").unwrap();
    let lock = directory.lock();
    let guard = lock.acquire("register health").unwrap();
    let mut request = request(&directory, "tests/one.in");
    request.answer = Some("tests/missing.ans".into());
    request.no_answer = false;
    assert!(
        store
            .register(&guard, &request)
            .unwrap_err()
            .message
            .contains("missing")
    );
    request.answer = None;
    fs::create_dir(directory.path("problems/A/tests/one.ans")).unwrap();
    assert!(
        store
            .register(&guard, &request)
            .unwrap_err()
            .message
            .contains("not a regular file")
    );
    assert!(!directory.path("problems/A/tests.toml").exists());
    assert_no_temps(&directory.path("problems/A"));
}

#[test]
fn a_guard_for_another_workspace_cannot_mutate_this_store() {
    let directory = Directory::new();
    let other = Directory::new();
    let store = setup(&directory);
    let capture = CapturedFile::from_reader(&mut &b"input"[..]).unwrap();
    let lock = WorkspaceLock::new(&other.0).unwrap();
    let guard = lock.acquire("wrong workspace").unwrap();
    assert!(
        store
            .register(&guard, &request(&directory, "tests/new.in"))
            .is_err()
    );
    assert!(store.set_enabled(&guard, "one", true).is_err());
    assert!(store.add(&guard, &capture, None, Origin::Custom).is_err());
    assert!(!directory.path("problems/A/tests/custom-1.in").exists());
    assert!(!directory.path("problems/A/tests.toml").exists());
    assert_no_temps(&directory.path("problems/A"));
}

#[cfg(feature = "storage-test-hooks")]
mod replay {
    use super::*;
    use cpforge::storage::{LockOptions, TestHooks};
    use std::path::Path;

    // Each hook is idempotent: only the first attempt changes the baseline.
    fn reorder(path: &Path) {
        let text = entry("two", "two.in") + &entry("one", "one.in");
        fs::write(path, text).unwrap();
    }

    fn remove(path: &Path) {
        fs::write(path, entry("two", "two.in")).unwrap();
    }

    fn take_id(path: &Path) {
        fs::write(path, entry("new", "other.in")).unwrap();
    }

    fn take_input(path: &Path) {
        fs::write(path, entry("other", "new.in")).unwrap();
    }

    fn take_answer(path: &Path) {
        fs::write(path, entry("other", "new.ans")).unwrap();
    }

    fn make_too_new(path: &Path) {
        fs::write(path, "schema = 2\ntest = false\n").unwrap();
    }

    fn make_malformed(path: &Path) {
        fs::write(path, "[[test]]\nid = 'broken'\n").unwrap();
    }

    fn take_casefolded_id(path: &Path) {
        fs::write(path, entry("NEW", "other.in")).unwrap();
    }

    #[test]
    fn enable_replay_uses_id_after_editor_reordered_entries() {
        let directory = Directory::new();
        let store = setup(&directory);
        let original = entry("one", "one.in") + &entry("two", "two.in");
        fs::write(directory.path("problems/A/tests.toml"), original).unwrap();
        let lock = directory.lock();
        let guard = lock
            .acquire_with_options(
                "reorder replay",
                LockOptions {
                    test_hooks: TestHooks {
                        edit_after_read: Some(reorder),
                        ..TestHooks::default()
                    },
                    ..LockOptions::default()
                },
            )
            .unwrap();
        assert!(store.set_enabled(&guard, "one", false).unwrap().value);
        let expected = entry("two", "two.in")
            + &entry("one", "one.in").replace("enabled = true", "enabled = false");
        assert_eq!(
            fs::read_to_string(directory.path("problems/A/tests.toml")).unwrap(),
            expected
        );
        assert_no_temps(&directory.path("problems/A"));
    }

    #[test]
    fn removal_during_replay_is_reported_and_editor_document_is_unchanged() {
        let directory = Directory::new();
        let store = setup(&directory);
        fs::write(
            directory.path("problems/A/tests.toml"),
            entry("one", "one.in"),
        )
        .unwrap();
        let lock = directory.lock();
        let guard = lock
            .acquire_with_options(
                "removal replay",
                LockOptions {
                    test_hooks: TestHooks {
                        edit_after_read: Some(remove),
                        ..TestHooks::default()
                    },
                    ..LockOptions::default()
                },
            )
            .unwrap();
        let error = store.set_enabled(&guard, "one", false).unwrap_err();
        assert!(
            error.message.contains("unknown registered test id"),
            "{error}"
        );
        assert_eq!(
            fs::read_to_string(directory.path("problems/A/tests.toml")).unwrap(),
            entry("two", "two.in")
        );
        assert_no_temps(&directory.path("problems/A"));
    }

    #[test]
    fn insertion_replay_rechecks_id_and_both_paths_and_fresh_classification() {
        let hooks: [fn(&Path); 6] = [
            take_id,
            take_casefolded_id,
            take_input,
            take_answer,
            make_too_new,
            make_malformed,
        ];
        for hook in hooks {
            let directory = Directory::new();
            let store = setup(&directory);
            fs::write(directory.path("problems/A/tests.toml"), "schema = 1\n").unwrap();
            fs::write(directory.path("problems/A/tests/new.in"), b"input").unwrap();
            fs::write(directory.path("problems/A/tests/new.ans"), b"answer").unwrap();
            let lock = directory.lock();
            let guard = lock
                .acquire_with_options(
                    "insertion replay",
                    LockOptions {
                        test_hooks: TestHooks {
                            edit_after_read: Some(hook),
                            ..TestHooks::default()
                        },
                        ..LockOptions::default()
                    },
                )
                .unwrap();
            let mut request = request(&directory, "tests/new.in");
            request.no_answer = false;
            let error = store.register(&guard, &request).unwrap_err();
            assert!(
                error.message.contains("conflict")
                    || error.message.contains("too_new")
                    || error.message.contains("malformed"),
                "{error}"
            );
            let actual = fs::read(directory.path("problems/A/tests.toml")).unwrap();
            // Render the hook's expected document independently in the fixture.
            let expected = directory.path("expected.toml");
            hook(&expected);
            assert_eq!(actual, fs::read(expected).unwrap());
            assert_eq!(
                fs::read(directory.path("problems/A/tests/new.in")).unwrap(),
                b"input"
            );
            assert_eq!(
                fs::read(directory.path("problems/A/tests/new.ans")).unwrap(),
                b"answer"
            );
            assert_no_temps(&directory.path("problems/A"));
        }
    }

    #[test]
    fn no_clobber_catalog_creation_classifies_a_concurrent_creator() {
        fn creator(path: &Path) {
            if path.file_name().and_then(|name| name.to_str()) == Some("tests.toml") {
                fs::write(path, "schema = 2\ntest = false\n").unwrap();
            }
        }
        let directory = Directory::new();
        let store = setup(&directory);
        fs::write(directory.path("problems/A/tests/new.in"), b"data").unwrap();
        let lock = directory.lock();
        let guard = lock
            .acquire_with_options(
                "catalog creation race",
                LockOptions {
                    test_hooks: TestHooks {
                        before_file_publish: Some(creator),
                        ..TestHooks::default()
                    },
                    ..LockOptions::default()
                },
            )
            .unwrap();
        assert!(
            store
                .register(&guard, &request(&directory, "tests/new.in"))
                .unwrap_err()
                .message
                .contains("too_new")
        );
        assert_eq!(
            fs::read(directory.path("problems/A/tests.toml")).unwrap(),
            b"schema = 2\ntest = false\n"
        );
        assert_no_temps(&directory.path("problems/A"));
    }

    #[test]
    fn creation_publication_warnings_survive_a_later_edit_failure() {
        let directory = Directory::new();
        let store = setup(&directory);
        fs::write(directory.path("problems/A/tests/new.in"), b"data").unwrap();
        let lock = directory.lock();
        let guard = lock
            .acquire_with_options(
                "publication warnings",
                LockOptions {
                    test_hooks: TestHooks {
                        post_commit_sync_failure: true,
                        edit_after_read: Some(make_too_new),
                        ..TestHooks::default()
                    },
                    ..LockOptions::default()
                },
            )
            .unwrap();
        let error = store
            .register(&guard, &request(&directory, "tests/new.in"))
            .unwrap_err();
        assert!(error.message.contains("too_new"));
        assert!(
            error
                .warnings
                .iter()
                .any(|warning| warning.contains("durability uncertain"))
        );
        assert_no_temps(&directory.path("problems/A"));
    }
}

#[cfg(unix)]
#[test]
fn catalog_read_links_are_not_writable() {
    use std::os::unix::fs::symlink;
    let directory = Directory::new();
    let store = setup(&directory);
    let text = entry("one", "missing.in");
    fs::write(directory.path("original.toml"), &text).unwrap();
    symlink(
        "../../original.toml",
        directory.path("problems/A/tests.toml"),
    )
    .unwrap();
    assert!(matches!(store.list().unwrap().catalog, Record::Valid(_)));
    let lock = directory.lock();
    let guard = lock.acquire("catalog link refusal").unwrap();
    assert!(
        store
            .set_enabled(&guard, "one", false)
            .unwrap_err()
            .message
            .contains("link")
    );
    assert_eq!(
        fs::read(directory.path("original.toml")).unwrap(),
        text.as_bytes()
    );
    assert_no_temps(&directory.path("problems/A"));
}

#[cfg(feature = "storage-test-hooks")]
mod add_transactions {
    use super::*;
    use cpforge::storage::{LockOptions, TestHooks};
    use cpforge::tests_store::AddStep;
    use std::path::Path;

    fn capture(mut bytes: &[u8]) -> CapturedFile {
        CapturedFile::from_reader(&mut bytes).unwrap()
    }

    fn collide_answer_and_edit_input(path: &Path) {
        if path.extension().and_then(|extension| extension.to_str()) == Some("ans") {
            fs::write(path.with_extension("in"), b"editor's input\0\r\n").unwrap();
            fs::write(path, b"other creator's answer\0\r\n").unwrap();
        }
    }

    #[test]
    fn answer_exists_keeps_edited_input_and_reports_only_committed_paths() {
        let directory = Directory::new();
        let store = setup(&directory);
        let original = "# keep catalog\nschema = 1\n";
        fs::write(directory.path("problems/A/tests.toml"), original).unwrap();
        let input = capture(b"input\0\r\n");
        let answer = capture(b"answer\0\r\n");
        let lock = directory.lock();
        let guard = lock
            .acquire_with_options(
                "answer collision",
                LockOptions {
                    test_hooks: TestHooks {
                        before_file_publish: Some(collide_answer_and_edit_input),
                        post_commit_sync_failure: true,
                        ..TestHooks::default()
                    },
                    ..LockOptions::default()
                },
            )
            .unwrap();
        let outcome = store
            .add(&guard, &input, Some(&answer), Origin::Custom)
            .unwrap();
        assert!(!outcome.registered);
        assert_eq!(outcome.id.as_str(), "custom-1");
        assert_eq!(
            outcome.published,
            [std::path::PathBuf::from("tests/custom-1.in")]
        );
        let failure = outcome.failure.unwrap();
        assert_eq!(failure.step, AddStep::AnswerPublication);
        assert!(failure.cause.contains("already exists"), "{failure:?}");
        assert!(
            outcome
                .warnings
                .iter()
                .any(|warning| warning.contains("durability uncertain"))
        );
        assert_eq!(
            fs::read(directory.path("problems/A/tests/custom-1.in")).unwrap(),
            b"editor's input\0\r\n"
        );
        assert_eq!(
            fs::read(directory.path("problems/A/tests/custom-1.ans")).unwrap(),
            b"other creator's answer\0\r\n"
        );
        assert_eq!(
            fs::read(directory.path("problems/A/tests.toml")).unwrap(),
            original.as_bytes()
        );
        assert_eq!(
            store.list().unwrap().candidates[0].input,
            "tests/custom-1.in"
        );
        assert_no_temps(&directory.path("problems/A/tests"));
    }

    fn take_id(path: &Path) {
        fs::write(path, entry("CUSTOM-1", "editor.in")).unwrap();
    }

    fn take_input(path: &Path) {
        fs::write(path, entry("editor", "CUSTOM-1.IN")).unwrap();
    }

    fn take_answer(path: &Path) {
        fs::write(path, entry("editor", "CUSTOM-1.ANS")).unwrap();
    }

    #[test]
    fn add_replay_rechecks_id_and_both_paths_and_keeps_all_published_bytes() {
        for hook in [take_id as fn(&Path), take_input, take_answer] {
            let directory = Directory::new();
            let store = setup(&directory);
            fs::write(directory.path("problems/A/tests.toml"), "schema = 1\n").unwrap();
            let input = capture(b"input\0\r\n");
            let answer = capture(b"answer\0\r\n");
            let lock = directory.lock();
            let guard = lock
                .acquire_with_options(
                    "add insertion replay",
                    LockOptions {
                        test_hooks: TestHooks {
                            edit_after_read: Some(hook),
                            ..TestHooks::default()
                        },
                        ..LockOptions::default()
                    },
                )
                .unwrap();
            let outcome = store
                .add(&guard, &input, Some(&answer), Origin::Custom)
                .unwrap();
            assert!(!outcome.registered);
            assert_eq!(outcome.published.len(), 2);
            let failure = outcome.failure.unwrap();
            assert_eq!(failure.step, AddStep::Registration);
            assert!(failure.cause.contains("conflict"), "{failure:?}");
            let expected = directory.path("expected.toml");
            hook(&expected);
            assert_eq!(
                fs::read(directory.path("problems/A/tests.toml")).unwrap(),
                fs::read(expected).unwrap()
            );
            assert_eq!(
                fs::read(directory.path("problems/A/tests/custom-1.in")).unwrap(),
                b"input\0\r\n"
            );
            assert_eq!(
                fs::read(directory.path("problems/A/tests/custom-1.ans")).unwrap(),
                b"answer\0\r\n"
            );
            assert_no_temps(&directory.path("problems/A"));
            assert_no_temps(&directory.path("problems/A/tests"));
        }
    }

    fn edit_input(path: &Path) {
        fs::write(
            path.parent().unwrap().join("tests/custom-1.in"),
            b"editor's new bytes\0\r\n",
        )
        .unwrap();
    }

    #[test]
    fn user_edits_between_publication_and_registration_are_registered_as_is() {
        let directory = Directory::new();
        let store = setup(&directory);
        let input = capture(b"captured input");
        let lock = directory.lock();
        let guard = lock
            .acquire_with_options(
                "edit published data",
                LockOptions {
                    test_hooks: TestHooks {
                        edit_after_read: Some(edit_input),
                        ..TestHooks::default()
                    },
                    ..LockOptions::default()
                },
            )
            .unwrap();
        let outcome = store.add(&guard, &input, None, Origin::Custom).unwrap();
        assert!(outcome.registered && outcome.failure.is_none());
        assert_eq!(
            fs::read(directory.path("problems/A/tests/custom-1.in")).unwrap(),
            b"editor's new bytes\0\r\n"
        );
        assert_eq!(store.list().unwrap().registered.len(), 1);
    }

    #[test]
    fn uncertain_publication_and_pending_cleanup_continue_through_registration() {
        for pending in [false, true] {
            let directory = Directory::new();
            let store = setup(&directory);
            if pending {
                // A pending link to a newly bootstrapped catalog would make it
                // multiply linked, which the edit primitive correctly refuses.
                fs::write(directory.path("problems/A/tests.toml"), "schema = 1\n").unwrap();
            }
            let input = capture(b"input");
            let answer = capture(b"answer");
            let lock = directory.lock();
            let guard = lock
                .acquire_with_options(
                    "publication warnings",
                    LockOptions {
                        test_hooks: TestHooks {
                            post_commit_sync_failure: true,
                            force_link_fallback: pending && cfg!(target_os = "linux"),
                            fail_unlink: pending,
                            ..TestHooks::default()
                        },
                        ..LockOptions::default()
                    },
                )
                .unwrap();
            let outcome = store
                .add(&guard, &input, Some(&answer), Origin::Custom)
                .unwrap();
            assert!(outcome.registered && outcome.failure.is_none());
            assert_eq!(outcome.published.len(), 2);
            assert_eq!(
                outcome
                    .warnings
                    .iter()
                    .filter(|warning| warning.contains("durability uncertain"))
                    .count(),
                if pending { 3 } else { 4 }
            );
            if pending && cfg!(target_os = "linux") {
                assert_eq!(
                    outcome
                        .warnings
                        .iter()
                        .filter(|warning| warning.contains("cleanup pending"))
                        .count(),
                    2
                );
            }
            drop(guard);
            let _guard = lock.acquire("recover link temporaries").unwrap();
            assert_no_temps(&directory.path("problems/A"));
            assert_no_temps(&directory.path("problems/A/tests"));
            assert_eq!(store.list().unwrap().registered.len(), 1);
        }
    }

    #[test]
    fn failed_first_publication_reports_no_paths_and_cleanup_warning() {
        let directory = Directory::new();
        let store = setup(&directory);
        let input = capture(b"input");
        let lock = directory.lock();
        let guard = lock
            .acquire_with_options(
                "input failure",
                LockOptions {
                    test_hooks: TestHooks {
                        fail_write_disk_full: true,
                        fail_unlink: true,
                        ..TestHooks::default()
                    },
                    ..LockOptions::default()
                },
            )
            .unwrap();
        let outcome = store.add(&guard, &input, None, Origin::Custom).unwrap();
        assert!(outcome.published.is_empty() && !outcome.registered);
        assert_eq!(outcome.failure.unwrap().step, AddStep::InputPublication);
        assert!(
            outcome
                .warnings
                .iter()
                .any(|warning| warning.contains("cleanup pending"))
        );
        assert!(!directory.path("problems/A/tests/custom-1.in").exists());
        assert!(!directory.path("problems/A/tests.toml").exists());
        drop(guard);
        let guard = lock.acquire("retry after input failure").unwrap();
        let outcome = store.add(&guard, &input, None, Origin::Custom).unwrap();
        assert!(outcome.registered && outcome.failure.is_none());
        assert_eq!(outcome.id.as_str(), "custom-1");
        assert_no_temps(&directory.path("problems/A/tests"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pending_catalog_link_is_reported_as_partial_then_recovered_before_rerun() {
        let directory = Directory::new();
        let store = setup(&directory);
        let input = capture(b"input");
        let lock = directory.lock();
        let guard = lock
            .acquire_with_options(
                "catalog pending link",
                LockOptions {
                    test_hooks: TestHooks {
                        force_link_fallback: true,
                        fail_unlink: true,
                        ..TestHooks::default()
                    },
                    ..LockOptions::default()
                },
            )
            .unwrap();
        let outcome = store.add(&guard, &input, None, Origin::Custom).unwrap();
        assert!(!outcome.registered);
        let failure = outcome.failure.unwrap();
        assert_eq!(failure.step, AddStep::Registration);
        assert!(failure.cause.contains("one link"), "{failure:?}");
        assert_eq!(outcome.published.len(), 1);
        assert_eq!(
            outcome
                .warnings
                .iter()
                .filter(|warning| warning.contains("cleanup pending"))
                .count(),
            2
        );
        assert_eq!(
            fs::read(directory.path("problems/A/tests.toml")).unwrap(),
            b"schema = 1\n"
        );
        drop(guard);
        let guard = lock.acquire("recover catalog pending link").unwrap();
        let outcome = store.add(&guard, &input, None, Origin::Custom).unwrap();
        assert!(outcome.registered && outcome.failure.is_none());
        assert_eq!(outcome.id.as_str(), "custom-2");
        assert_eq!(
            fs::read(directory.path("problems/A/tests/custom-1.in")).unwrap(),
            b"input"
        );
        assert_eq!(store.list().unwrap().candidates.len(), 1);
        assert_no_temps(&directory.path("problems/A"));
        assert_no_temps(&directory.path("problems/A/tests"));
    }

    #[test]
    fn edit_failure_retains_first_attempt_cleanup_warning_on_replay_conflict() {
        let directory = Directory::new();
        let store = setup(&directory);
        fs::write(directory.path("problems/A/tests.toml"), "schema = 1\n").unwrap();
        let input = capture(b"input");
        let lock = directory.lock();
        let guard = lock
            .acquire_with_options(
                "edit cleanup warnings",
                LockOptions {
                    test_hooks: TestHooks {
                        edit_after_read: Some(take_id),
                        fail_unlink: true,
                        ..TestHooks::default()
                    },
                    ..LockOptions::default()
                },
            )
            .unwrap();
        let outcome = store.add(&guard, &input, None, Origin::Custom).unwrap();
        assert!(!outcome.registered);
        assert_eq!(outcome.failure.unwrap().step, AddStep::Registration);
        assert!(
            outcome
                .warnings
                .iter()
                .any(|warning| warning.contains("cleanup pending"))
        );
        assert_eq!(
            fs::read(directory.path("problems/A/tests/custom-1.in")).unwrap(),
            b"input"
        );
    }
}
