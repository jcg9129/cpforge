use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Value, json};

use super::*;

fn file(kind: LayerKind, platform: Platform) -> PathBuf {
    let name = match kind {
        LayerKind::BuiltIn => "built-in",
        LayerKind::DefaultUser | LayerKind::OverrideUser => "user.toml",
        LayerKind::Workspace => "workspace.toml",
        LayerKind::Problem => "problem.toml",
    };
    let directory = match kind {
        LayerKind::DefaultUser | LayerKind::OverrideUser => "user",
        LayerKind::Workspace => "workspace",
        LayerKind::Problem => "problem",
        LayerKind::BuiltIn => "builtin",
    };
    PathBuf::from(if platform == Platform::Windows {
        format!(r"C:\{directory}\{name}")
    } else {
        format!("/{directory}/{name}")
    })
}
fn layer(text: &str, kind: LayerKind, platform: Platform) -> RawLayer {
    parse(
        text.as_bytes(),
        &LayerSource {
            kind,
            file: file(kind, platform),
        },
        &Environment::default(),
        platform,
    )
}
fn merged(texts: &[(&str, LayerKind)], platform: Platform) -> MergedConfig {
    let layers: Vec<_> = texts
        .iter()
        .map(|(text, kind)| layer(text, *kind, platform))
        .collect();
    let result = compose(&layers, platform);
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    result
}
fn value(node: &EffectiveNode) -> Value {
    plain(&node.value)
}
fn plain(value: &EffectiveValue) -> Value {
    match value {
        EffectiveValue::Table(t) => {
            Value::Object(t.iter().map(|(k, v)| (k.clone(), self::value(v))).collect())
        }
        EffectiveValue::Array(a) => Value::Array(a.iter().map(self::value).collect()),
        EffectiveValue::Scalar(raw) => match raw {
            RawValue::String(s) | RawValue::Datetime(s) => json!(s),
            RawValue::Integer(i) => json!(i),
            RawValue::Float(f) => json!(f),
            RawValue::Boolean(b) => json!(b),
            _ => panic!("effective scalar contains a container"),
        },
    }
}
fn path(keys: &[&str]) -> Vec<String> {
    keys.iter().map(|s| (*s).to_owned()).collect()
}
fn origin(
    node: &EffectiveNode,
    kind: LayerKind,
    platform: Platform,
    line: usize,
    col: usize,
    key: &str,
    overlay: Option<&str>,
) {
    assert_eq!(
        node.provenance,
        Provenance::File {
            path: file(kind, platform),
            line: Some(line),
            col: Some(col)
        }
    );
    assert_eq!(node.origin.file, file(kind, platform));
    assert_eq!((node.origin.line, node.origin.col), (Some(line), Some(col)));
    assert_eq!(node.origin.key_path, key);
    assert_eq!(node.overlay.as_deref(), overlay);
}
fn origins_in(node: &EffectiveNode, kind: LayerKind, platform: Platform) {
    match &node.value {
        EffectiveValue::Table(t) => {
            for child in t.values() {
                origins_in(child, kind, platform);
            }
        }
        EffectiveValue::Array(a) => {
            for child in a {
                origins_in(child, kind, platform);
            }
        }
        EffectiveValue::Scalar(_) => {
            assert_eq!(
                node.provenance,
                Provenance::File {
                    path: file(kind, platform),
                    line: node.origin.line,
                    col: node.origin.col
                }
            );
            assert_eq!(node.overlay, None);
        }
    }
}

#[test]
fn unit_replacements_have_exact_values_and_origins() {
    for (first, second, keys, expected) in [
        (
            "[run]\nargv = ['old']",
            "[run]\nshell = { interpreter = ['sh', '-c'], script = 'new' }",
            vec!["run", "shell"],
            json!({"interpreter":["sh","-c"],"script":"new"}),
        ),
        (
            "[run]\nshell = { interpreter = ['sh', '-c'], script = 'old' }",
            "[run]\nargv = ['new', 'arg']",
            vec!["run", "argv"],
            json!(["new", "arg"]),
        ),
        (
            "[run]\nshell = { interpreter = ['sh', '-c'], script = 'old', args = ['old'] }",
            "[run]\nshell = { args = ['new'] }",
            vec!["run", "shell"],
            json!({"args":["new"]}),
        ),
        (
            "[languages.demo]\nartifacts = { file = 'old' }",
            "[languages.demo]\nartifacts = { dir = true, required = ['New.class'] }",
            vec!["languages", "demo", "artifacts"],
            json!({"dir":true,"required":["New.class"]}),
        ),
        (
            "[languages.demo]\nartifacts = { dir = true, required = ['Old.class'] }",
            "[languages.demo]\nartifacts = { file = 'new' }",
            vec!["languages", "demo", "artifacts"],
            json!({"file":"new"}),
        ),
        (
            "[run]\nlimits = { address_space = 10, job_commit = 20 }",
            "[run]\nlimits = {}",
            vec!["run", "limits"],
            json!({}),
        ),
        (
            "[run]\nlimits = { address_space = 10 }",
            "[run]\nlimits = { job_commit = 20 }",
            vec!["run", "limits"],
            json!({"job_commit":20}),
        ),
        (
            "[run]\ncwd = { path = 'a' }",
            "[run]\ncwd = { selector = 'problem' }",
            vec!["run", "cwd"],
            json!({"selector":"problem"}),
        ),
        (
            "[run]\ncwd = { selector = 'problem' }",
            "[run]\ncwd = { path = 'a' }",
            vec!["run", "cwd"],
            json!({"path":"a"}),
        ),
        (
            "compare = { float = 1 }",
            "compare = { exact = true }",
            vec!["compare"],
            json!({"exact":true}),
        ),
        (
            "[languages.demo]\ntemplate = { path = 'old' }",
            "[languages.demo]\ntemplate = { path = 'new' }",
            vec!["languages", "demo", "template"],
            json!({"path":"new"}),
        ),
        (
            "[run]\nenv_remove = ['X', 'Y']",
            "[run]\nenv_remove = ['Z']",
            vec!["run", "env_remove"],
            json!(["Z"]),
        ),
        (
            "[languages.demo]\nsources = ['old', 'other']",
            "[languages.demo]\nsources = []",
            vec!["languages", "demo", "sources"],
            json!([]),
        ),
    ] {
        for platform in [Platform::Linux, Platform::Macos, Platform::Windows] {
            let result = merged(
                &[
                    (first, LayerKind::DefaultUser),
                    (second, LayerKind::Workspace),
                ],
                platform,
            );
            let node = result.at(&keys).unwrap();
            assert_eq!(value(node), expected, "{platform:?}: {first} -> {second}");
            let line = if keys == ["compare"] { 1 } else { 2 };
            origin(
                node,
                LayerKind::Workspace,
                platform,
                line,
                1,
                &keys.join("."),
                None,
            );
            origins_in(node, LayerKind::Workspace, platform);
            assert!(node.markers.is_empty());
            assert!(!node.reset_occurred);
            assert_eq!(node.events.last().unwrap().provenance, node.provenance);
            let previous = layer(first, LayerKind::DefaultUser, platform);
            if let Some(old) = previous.state.loaded_at(&keys) {
                let event = node.events.last().unwrap();
                assert_eq!(event.kind, EventKind::Replace);
                assert_eq!(event.removed.len(), 1);
                assert_eq!(event.removed[0].origin, old.origin);
            } else {
                let other = if keys == ["run", "argv"] {
                    "shell"
                } else {
                    "argv"
                };
                let old = previous.state.loaded_at(&["run", other]).unwrap();
                let event = node.events.last().unwrap();
                assert_eq!(event.kind, EventKind::Replace);
                assert_eq!(event.removed.len(), 1);
                assert_eq!(event.removed[0].origin, old.origin);
                assert_eq!(
                    result.history[&path(&["run", other])].last().unwrap().kind,
                    EventKind::Discarded
                );
            }
            if keys == ["run", "argv"] {
                assert!(result.at(&["run", "shell"]).is_none());
            }
            if keys == ["run", "shell"] {
                assert!(result.at(&["run", "argv"]).is_none());
            }
        }
    }
}

// Raw-only lookup for assertions about source spans, independent of composition.
trait LoadedAt {
    fn loaded_at(&self, keys: &[&str]) -> Option<&RawNode>;
}
impl LoadedAt for LayerState {
    fn loaded_at(&self, keys: &[&str]) -> Option<&RawNode> {
        let Self::Loaded(root) = self else {
            return None;
        };
        keys.iter().try_fold(root, |node, key| node.get(key))
    }
}

#[test]
fn reset_discards_fields_and_retains_its_fact_after_reintroduction() {
    for prefix in ["languages.demo", "languages.demo.profiles.p"] {
        let first =
            format!("[{prefix}.build]\nargv = ['old']\nenv = {{ A = 'old' }}\ndeadline_s = 1");
        let reset = format!("[{prefix}]\nbuild = false");
        let after = format!("[{prefix}.build]\nargv = ['fresh']");
        let mut keys: Vec<_> = prefix.split('.').collect();
        keys.push("build");
        let result = merged(
            &[
                (&first, LayerKind::DefaultUser),
                (&reset, LayerKind::Workspace),
            ],
            Platform::Linux,
        );
        let node = result.at(&keys).unwrap();
        assert_eq!(value(node), json!(false));
        assert!(node.reset_occurred);
        origin(
            node,
            LayerKind::Workspace,
            Platform::Linux,
            2,
            1,
            &format!("{prefix}.build"),
            None,
        );
        assert_eq!(
            node.events.iter().map(|e| e.kind).collect::<Vec<_>>(),
            [EventKind::Set, EventKind::Reset]
        );
        assert_eq!(
            plain(&node.events[1].removed[0].value),
            json!({"argv":["old"],"env":{"A":"old"},"deadline_s":1})
        );
        // Two workspace contributions exercise a reset followed by a table in
        // the same precedence row; input order within that layer rank is stable.
        let result = merged(
            &[
                (&first, LayerKind::DefaultUser),
                (&reset, LayerKind::Workspace),
                (&after, LayerKind::Workspace),
            ],
            Platform::Linux,
        );
        let node = result.at(&keys).unwrap();
        assert_eq!(value(node), json!({"argv":["fresh"]}));
        assert!(node.reset_occurred);
        assert_eq!(
            node.events.iter().map(|e| e.kind).collect::<Vec<_>>(),
            [EventKind::Set, EventKind::Reset, EventKind::Replace]
        );
        assert_eq!(plain(&node.events[2].removed[0].value), json!(false));
        let mut discarded = path(&keys);
        discarded.extend(["env".into(), "A".into()]);
        let event = result.history[&discarded].last().unwrap();
        assert_eq!(event.kind, EventKind::Discarded);
        assert_eq!(plain(&event.removed[0].value), json!("old"));
    }
    // A reset resolved inside one layer must also wipe an earlier layer's fields.
    let result = merged(
        &[
            (
                "[languages.demo.build]\nenv = { A = 'old' }\nargv = ['old']",
                LayerKind::DefaultUser,
            ),
            (
                "[languages.demo]\nbuild = false\n[languages.demo.windows.build]\nargv = ['fresh']",
                LayerKind::Workspace,
            ),
        ],
        Platform::Windows,
    );
    let build = result.language("demo").unwrap().build().unwrap();
    assert_eq!(value(build.node), json!({"argv":["fresh"]}));
    assert!(build.node.reset_occurred);
    origin(
        build.node.get("argv").unwrap(),
        LayerKind::Workspace,
        Platform::Windows,
        4,
        1,
        "languages.demo.windows.build.argv",
        Some("languages.demo.windows"),
    );
    let argv = build.argv().unwrap();
    assert_eq!(
        argv.events
            .iter()
            .map(|event| event.kind)
            .collect::<Vec<_>>(),
        [EventKind::Set, EventKind::Discarded, EventKind::Set]
    );
    assert_eq!(argv.events[1].origin.key_path, "languages.demo.build");
    assert_eq!(argv.events[1].origin.line, Some(2));
    let reset = build
        .node
        .events
        .iter()
        .find(|event| event.kind == EventKind::Reset)
        .unwrap();
    assert_eq!(
        plain(&reset.removed[0].value),
        json!({"argv":["old"],"env":{"a":"old"}})
    );
}

#[test]
fn env_tombstone_survives_middle_layer_and_can_be_set_again() {
    for platform in [Platform::Linux, Platform::Windows] {
        let texts = [
            (
                "[languages.demo.run]\nenv = { X = '1' }",
                LayerKind::DefaultUser,
            ),
            (
                "[languages.demo.run]\nenv = { X = false }",
                LayerKind::Workspace,
            ),
            ("[languages.demo.run]\ndeadline_s = 1", LayerKind::Workspace),
        ];
        let key = if platform == Platform::Windows {
            "x"
        } else {
            "X"
        };
        let result = merged(&texts, platform);
        let node = result
            .at(&["languages", "demo", "run", "env", key])
            .unwrap();
        assert_eq!(value(node), json!(false));
        assert!(node.is_tombstone());
        origin(
            node,
            LayerKind::Workspace,
            platform,
            2,
            9,
            "languages.demo.run.env.X",
            None,
        );
        assert_eq!(
            node.events.iter().map(|e| e.kind).collect::<Vec<_>>(),
            [EventKind::Set, EventKind::Delete]
        );
        assert_eq!(plain(&node.events[1].removed[0].value), json!("1"));
        let mut texts = texts.to_vec();
        texts.push((
            "[languages.demo.run]\nenv = { X = '2' }",
            LayerKind::Workspace,
        ));
        let result = merged(&texts, platform);
        let node = result
            .at(&["languages", "demo", "run", "env", key])
            .unwrap();
        assert_eq!(value(node), json!("2"));
        assert!(!node.is_tombstone());
        assert_eq!(
            node.events.iter().map(|e| e.kind).collect::<Vec<_>>(),
            [EventKind::Set, EventKind::Delete, EventKind::Replace]
        );
        assert_eq!(plain(&node.events[2].removed[0].value), json!(false));
        assert_eq!(
            node.events[2].removed[0].provenance,
            node.events[1].provenance
        );
    }
    let result = merged(
        &[
            (
                "[run]\nenv = { Path = 'old', X = 'kept' }",
                LayerKind::DefaultUser,
            ),
            ("[run]\nenv = { PATH = false }", LayerKind::Workspace),
            ("[run]\nenv = { pAtH = 'new' }", LayerKind::Workspace),
        ],
        Platform::Windows,
    );
    let env = result.run().unwrap().env().unwrap();
    assert_eq!(
        env.keys().map(String::as_str).collect::<Vec<_>>(),
        ["path", "x"]
    );
    assert_eq!(value(&env["path"]), json!("new"));
    assert_eq!(value(&env["x"]), json!("kept"));
    assert_eq!(env["path"].name, "pAtH");
    assert_eq!(env["x"].name, "X");
    assert_eq!(
        result
            .run()
            .unwrap()
            .env_entries()
            .map(|(name, _)| name)
            .collect::<Vec<_>>(),
        ["pAtH", "X"]
    );
    origin(
        &env["path"],
        LayerKind::Workspace,
        Platform::Windows,
        2,
        9,
        "run.env.pAtH",
        None,
    );
    origin(
        &env["x"],
        LayerKind::DefaultUser,
        Platform::Windows,
        2,
        23,
        "run.env.X",
        None,
    );
    assert_eq!(
        env["path"]
            .events
            .iter()
            .map(|e| e.kind)
            .collect::<Vec<_>>(),
        [EventKind::Set, EventKind::Delete, EventKind::Replace]
    );
}

#[test]
fn overlays_are_inner_first_and_later_generic_wins() {
    let text = "[languages.demo.run]\nargv = ['generic']\n[languages.demo.run.unix]\nargv = ['unix']\n[languages.demo.run.linux]\nargv = ['linux']\n[languages.demo.run.windows]\nargv = ['a']\n[languages.demo.windows.run]\nargv = ['b']\n[commands.linux]\nargv = ['command']\n[commands.linux.windows]\nargv = ['windows-command']";
    for (platform, expected, line, supplied, overlay, command) in [
        (
            Platform::Linux,
            "linux",
            6,
            "languages.demo.run.linux.argv",
            "languages.demo.run.linux",
            "command",
        ),
        (
            Platform::Macos,
            "unix",
            4,
            "languages.demo.run.unix.argv",
            "languages.demo.run.unix",
            "command",
        ),
        (
            Platform::Windows,
            "b",
            10,
            "languages.demo.windows.run.argv",
            "languages.demo.windows",
            "windows-command",
        ),
    ] {
        let result = merged(&[(text, LayerKind::DefaultUser)], platform);
        let node = result.at(&["languages", "demo", "run", "argv"]).unwrap();
        assert_eq!(value(node), json!([expected]));
        origin(
            node,
            LayerKind::DefaultUser,
            platform,
            line,
            1,
            supplied,
            Some(overlay),
        );
        let element = &node.array().unwrap()[0];
        origin(
            element,
            LayerKind::DefaultUser,
            platform,
            line,
            9,
            &format!("{supplied}[0]"),
            Some(overlay),
        );
        assert_eq!(
            value(result.at(&["commands", "linux", "argv"]).unwrap()),
            json!([command])
        );
        assert!(result.at(&["languages", "demo", "windows"]).is_none());
        let result = merged(
            &[
                (text, LayerKind::DefaultUser),
                (
                    "[languages.demo.run]\nargv = ['later']",
                    LayerKind::Workspace,
                ),
            ],
            platform,
        );
        let node = result.at(&["languages", "demo", "run", "argv"]).unwrap();
        assert_eq!(value(node), json!(["later"]));
        origin(
            node,
            LayerKind::Workspace,
            platform,
            2,
            1,
            "languages.demo.run.argv",
            None,
        );
        assert_eq!(node.events.last().unwrap().kind, EventKind::Replace);
        assert_eq!(
            node.events.last().unwrap().removed[0].overlay.as_deref(),
            Some(overlay)
        );
    }
}

#[test]
fn overlay_unit_replacement_and_deletion_match_layer_rules() {
    let text = "[languages.demo]\nartifacts = { file = 'old' }\n[languages.demo.build]\nargv = ['old']\nenv = { X = 'old' }\n[languages.demo.windows]\nbuild = false\nartifacts = { dir = true }\n[languages.demo.run]\nargv = ['generic']\ncwd = { path = 'old' }\nlimits = { address_space = 1 }\nenv = { X = 'old' }\n[languages.demo.run.windows]\nshell = { interpreter = ['sh', '-c'], script = 'win' }\ncwd = { selector = 'problem' }\nlimits = {}\nenv = { X = false }";
    let result = merged(&[(text, LayerKind::Workspace)], Platform::Windows);
    let language = result.language("demo").unwrap();
    assert_eq!(value(language.artifacts().unwrap()), json!({"dir":true}));
    assert_eq!(value(language.build().unwrap().node), json!(false));
    assert!(language.build().unwrap().node.reset_occurred);
    assert_eq!(
        value(language.run().unwrap().node),
        json!({"shell":{"interpreter":["sh","-c"],"script":"win"},"cwd":{"selector":"problem"},"limits":{},"env":{"x":false}})
    );
    let node = result
        .at(&["languages", "demo", "run", "env", "x"])
        .unwrap();
    origin(
        node,
        LayerKind::Workspace,
        Platform::Windows,
        18,
        9,
        "languages.demo.run.windows.env.X",
        Some("languages.demo.run.windows"),
    );
    assert_eq!(node.events.last().unwrap().kind, EventKind::Delete);
    let result = merged(&[(text, LayerKind::Workspace)], Platform::Linux);
    assert_eq!(
        value(result.at(&["languages", "demo", "run"]).unwrap()),
        json!({"argv":["generic"],"cwd":{"path":"old"},"limits":{"address_space":1},"env":{"X":"old"}})
    );
    assert!(result.at(&["languages", "demo", "run", "shell"]).is_none());
}

#[test]
fn raw_and_composed_exclusivity_are_not_reported_twice() {
    let valid_text = "[run]\nargv = ['generic']\n[run.windows]\nshell = { interpreter = ['sh', '-c'], script = 'windows' }";
    for platform in [Platform::Linux, Platform::Macos, Platform::Windows] {
        let result = merged(&[(valid_text, LayerKind::Workspace)], platform);
        let stage = result.run().unwrap();
        if platform == Platform::Windows {
            assert_eq!(
                value(stage.node),
                json!({"shell":{"interpreter":["sh","-c"],"script":"windows"}})
            );
            assert!(matches!(stage.command(), Some(CommandChoice::Shell(_))));
        } else {
            assert_eq!(value(stage.node), json!({"argv":["generic"]}));
            assert!(matches!(stage.command(), Some(CommandChoice::Argv(_))));
            assert!(
                !result
                    .history
                    .keys()
                    .any(|key| key.starts_with(&path(&["run", "shell"])))
            );
        }
    }
    for text in [
        "[run]\nargv = ['x']\nshell = { script = 's' }\n[run.windows]\ndeadline_s = 1",
        "[run]\ncwd = { path = 'a', selector = 'run' }",
        "[languages.demo]\nartifacts = { file = 'a', dir = true, required = ['a'] }",
    ] {
        let raw = layer(text, LayerKind::Workspace, Platform::Windows);
        let expected = raw.diagnostics.clone();
        assert!(expected.iter().all(|d| d.second_origin.is_some()));
        let result = compose(&[raw], Platform::Windows);
        assert_eq!(result.diagnostics, expected);
        assert!(!result.markers.is_empty());
    }
}

#[test]
fn config_relative_paths_keep_their_defining_file_when_set_again() {
    let first = "[languages.demo]\ntemplate = { path = 'templates/a' }";
    let middle = "[languages.demo]\ntemplate = { path = 'templates/b' }";
    let result = merged(
        &[
            (first, LayerKind::DefaultUser),
            (middle, LayerKind::Workspace),
            (first, LayerKind::Workspace),
        ],
        Platform::Linux,
    );
    let template = result.language("demo").unwrap().template().unwrap();
    let leaf = template.get("path").unwrap();
    origin(
        leaf,
        LayerKind::Workspace,
        Platform::Linux,
        2,
        14,
        "languages.demo.template.path",
        None,
    );
    assert_eq!(
        leaf.typed,
        Some(TypedValue::Path("/workspace/templates/a".into()))
    );
    let events = &template.events;
    assert_eq!(
        events.iter().map(|e| e.kind).collect::<Vec<_>>(),
        [EventKind::Set, EventKind::Replace, EventKind::Replace]
    );
    let removed = &events[1].removed[0];
    assert_eq!(
        removed.provenance,
        Provenance::File {
            path: file(LayerKind::DefaultUser, Platform::Linux),
            line: Some(2),
            col: Some(1)
        }
    );
    let EffectiveValue::Table(t) = &removed.value else {
        panic!()
    };
    assert_eq!(
        t["path"].typed,
        Some(TypedValue::Path("/user/templates/a".into()))
    );
    assert_eq!(
        t["path"].origin.file,
        file(LayerKind::DefaultUser, Platform::Linux)
    );
    // A compiler-only override leaves the original template untouched.
    let result = merged(
        &[
            (first, LayerKind::DefaultUser),
            (
                "[languages.demo.build]\nargv = ['cc']",
                LayerKind::Workspace,
            ),
        ],
        Platform::Linux,
    );
    origin(
        result
            .at(&["languages", "demo", "template", "path"])
            .unwrap(),
        LayerKind::DefaultUser,
        Platform::Linux,
        2,
        14,
        "languages.demo.template.path",
        None,
    );
}

#[test]
fn invalid_markers_survive_overrides_removal_and_inactive_branches() {
    for (first, second, effective_path, marker_path, expected) in [
        (
            "[run]\ndeadline_s = false",
            "[run]\ndeadline_s = 2",
            vec!["run", "deadline_s"],
            vec!["run", "deadline_s"],
            json!(2),
        ),
        (
            "[run]\nargv = [1]",
            "[run]\nshell = { script = 's' }",
            vec!["run"],
            vec!["run", "argv", "0"],
            json!({"shell":{"script":"s"}}),
        ),
        (
            "[run]\nlimits = { address_space = false }",
            "[run]\nlimits = {}",
            vec!["run", "limits"],
            vec!["run", "limits", "address_space"],
            json!({}),
        ),
    ] {
        let layers = [
            layer(first, LayerKind::DefaultUser, Platform::Linux),
            layer(second, LayerKind::Workspace, Platform::Linux),
        ];
        let expected_diagnostics: Vec<_> =
            layers.iter().flat_map(|l| l.diagnostics.clone()).collect();
        let result = compose(&layers, Platform::Linux);
        assert_eq!(result.diagnostics, expected_diagnostics);
        assert_eq!(result.markers.len(), 1);
        let marker = &result.markers[0];
        assert_eq!(marker.path, path(&marker_path));
        assert_eq!(marker.layer, LayerKind::DefaultUser);
        assert!(marker.applies && !marker.rejected_key);
        assert_eq!(marker.origin, expected_diagnostics[0].origin);
        assert_eq!(
            marker.provenance,
            Provenance::File {
                path: file(LayerKind::DefaultUser, Platform::Linux),
                line: Some(2),
                col: marker.origin.col
            }
        );
        let node = result.at(&effective_path).unwrap();
        assert_eq!(value(node), expected);
        assert_eq!(node.markers, result.markers);
    }
    // A type error in an inactive branch is reported and kept, but it is not
    // an input on the validation platform and marks no effective node.
    let layers = [
        layer(
            "[run.windows]\nargv = [1]",
            LayerKind::DefaultUser,
            Platform::Linux,
        ),
        layer(
            "[run]\nargv = ['ok']",
            LayerKind::Workspace,
            Platform::Linux,
        ),
    ];
    let result = compose(&layers, Platform::Linux);
    assert_eq!(result.diagnostics, layers[0].diagnostics);
    assert_eq!(result.markers.len(), 1);
    assert_eq!(result.markers[0].path, path(&["run", "argv", "0"]));
    assert_eq!(result.markers[0].overlay.as_deref(), Some("run.windows"));
    assert!(!result.markers[0].applies);
    let run = result.run().unwrap();
    assert_eq!(value(run.node), json!({"argv":["ok"]}));
    assert!(run.node.markers.is_empty());
    assert!(run.argv().unwrap().markers.is_empty());
    assert!(result.root.markers.is_empty());
    let raw = layer(
        "[run.unix.linux]\nargv = ['forbidden']",
        LayerKind::Workspace,
        Platform::Linux,
    );
    let expected = raw.diagnostics.clone();
    let result = compose(&[raw], Platform::Linux);
    assert_eq!(result.diagnostics, expected);
    assert!(result.at(&["run", "argv"]).is_none());
    assert!(result.at(&["run", "linux"]).is_none());
    assert_eq!(result.markers[0].overlay.as_deref(), Some("run.unix.linux"));
    assert!(!result.markers[0].applies);
    assert!(result.root.markers.is_empty());
}

#[test]
fn invalid_layer_is_unknown_input_while_other_layers_still_compose() {
    let bad = layer("schema = 2", LayerKind::DefaultUser, Platform::Linux);
    let good = layer(
        "[run]\nargv = ['ok']",
        LayerKind::Workspace,
        Platform::Linux,
    );
    let result = compose(&[good, bad.clone()], Platform::Linux);
    assert_eq!(result.diagnostics, bad.diagnostics);
    assert_eq!(value(result.run().unwrap().node), json!({"argv":["ok"]}));
    assert_eq!(
        result.unknown_inputs,
        vec![UnknownInput {
            layer: LayerKind::DefaultUser,
            origin: Origin {
                file: file(LayerKind::DefaultUser, Platform::Linux),
                line: None,
                col: None,
                key_path: String::new(),
                span: None
            },
            provenance: Provenance::File {
                path: file(LayerKind::DefaultUser, Platform::Linux),
                line: None,
                col: None
            },
        }]
    );
}

#[test]
fn built_in_layer_is_valid_on_every_platform_and_has_no_stage_defaults() {
    for platform in [Platform::Linux, Platform::Macos, Platform::Windows] {
        let builtin = built_in(platform);
        assert_eq!(builtin.kind, LayerKind::BuiltIn);
        assert!(builtin.diagnostics.is_empty(), "{:?}", builtin.diagnostics);
        let result = compose(&[], platform);
        assert!(result.diagnostics.is_empty());
        assert!(result.markers.is_empty());
        assert!(result.unknown_inputs.is_empty());
        assert_eq!(
            result
                .languages()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["cpp", "java", "python"]
        );
        assert_eq!(
            value(result.at(&["languages", "cpp", "build"]).unwrap()),
            json!({"argv":["g++","-std=gnu++23","-O2","-Wall","{sources}","-o","{artifact}"]})
        );
        assert_eq!(
            value(result.at(&["languages", "java"]).unwrap()),
            json!({"extensions":["java"],"sources":["Main.java"],"entry":"Main","artifacts":{"dir":true},"build":{"argv":["javac","-d","{artifact_dir}","{sources}"]},"run":{"argv":["java","-cp","{artifact_dir}","{entry}"]}})
        );
        let language = result.language("python").unwrap();
        assert!(language.build().unwrap().node.reset_occurred);
        let argv = language.run().unwrap().node.get("argv").unwrap();
        assert_eq!(
            value(argv),
            json!([
                if platform == Platform::Windows {
                    "python"
                } else {
                    "python3"
                },
                "{source}"
            ])
        );
        assert_eq!(
            argv.overlay.as_deref(),
            if platform == Platform::Windows {
                Some("languages.python.run.windows")
            } else {
                None
            }
        );
        fn check(node: &EffectiveNode) {
            assert_eq!(node.provenance, Provenance::BuiltIn);
            assert!(node.markers.is_empty());
            match &node.value {
                EffectiveValue::Table(t) => {
                    for child in t.values() {
                        check(child);
                    }
                }
                EffectiveValue::Array(a) => {
                    for child in a {
                        check(child);
                    }
                }
                EffectiveValue::Scalar(_) => {}
            }
        }
        check(&result.root);
        let explicit = compose(&[builtin], platform);
        assert_eq!(explicit, result);
    }
}

#[test]
fn rows_stay_separate_and_public_views_preserve_partial_presence() {
    let result = merged(
        &[
            (
                "default_language = 'demo'\ncompare = { tokens = true }\n[build]\nargv = ['global']\n[run]\ndeadline_s = 1\n[languages.demo.build]\nenv = { A = 'language' }\n[languages.demo.profiles.p.build]\ncwd = { selector = 'problem' }\n[commands.linux]\nargv = ['cmd']\n[retention]\nkeep_jobs = 10\n[workspace]\nignore = ['*.o']\n[hooks]\non_switch = ['linux']",
                LayerKind::DefaultUser,
            ),
            (
                "[problem]\nid = 'local:p'\n[config]\nlanguage = 'demo'\nsources = ['p.cpp']\ncompare = { exact = true }\n[config.build]\ndeadline_s = 7\n[config.run.windows]\nargv = ['problem-win']",
                LayerKind::Problem,
            ),
        ],
        Platform::Windows,
    );
    assert_eq!(
        value(result.build().unwrap().node),
        json!({"argv":["global"]})
    );
    let language = result.language("demo").unwrap();
    assert_eq!(
        value(language.build().unwrap().node),
        json!({"env":{"a":"language"}})
    );
    let profile = language.profile("p").unwrap();
    assert_eq!(
        value(profile.build().unwrap().node),
        json!({"cwd":{"selector":"problem"}})
    );
    assert!(profile.run().is_none());
    assert_eq!(language.profiles().unwrap().len(), 1);
    assert_eq!(
        value(result.problem().unwrap().node),
        json!({"language":"demo","sources":["p.cpp"],"compare":{"exact":true},"build":{"deadline_s":7},"run":{"argv":["problem-win"]}})
    );
    assert_eq!(value(result.run().unwrap().node), json!({"deadline_s":1}));
    assert_eq!(result.commands().unwrap().len(), 1);
    assert_eq!(
        value(result.command("linux").unwrap().node),
        json!({"argv":["cmd"]})
    );
    assert_eq!(
        value(result.retention().unwrap().node),
        json!({"keep_jobs":10})
    );
    assert_eq!(value(result.workspace_ignore().unwrap()), json!(["*.o"]));
    assert_eq!(
        value(result.hooks().unwrap().node),
        json!({"on_switch":["linux"]})
    );
    assert_eq!(value(result.compare().unwrap()), json!({"tokens":true}));
    assert_eq!(result.default_language().unwrap().string(), Some("demo"));
    let node = result.at(&["config", "run", "argv"]).unwrap();
    origin(
        node,
        LayerKind::Problem,
        Platform::Windows,
        10,
        1,
        "config.run.windows.argv",
        Some("config.run.windows"),
    );
}

#[test]
fn nested_site_order_uses_generic_on_other_platforms() {
    let text = "[languages.demo.run]\nargv = ['generic']\n[languages.demo.run.windows]\nargv = ['a']\n[languages.demo.windows.run]\nargv = ['b']";
    for (platform, expected, line, key, overlay) in [
        (
            Platform::Linux,
            "generic",
            2,
            "languages.demo.run.argv",
            None,
        ),
        (
            Platform::Macos,
            "generic",
            2,
            "languages.demo.run.argv",
            None,
        ),
        (
            Platform::Windows,
            "b",
            6,
            "languages.demo.windows.run.argv",
            Some("languages.demo.windows"),
        ),
    ] {
        let result = merged(&[(text, LayerKind::Workspace)], platform);
        let node = result
            .language("demo")
            .unwrap()
            .run()
            .unwrap()
            .argv()
            .unwrap();
        assert_eq!(value(node), json!([expected]));
        origin(node, LayerKind::Workspace, platform, line, 1, key, overlay);
    }
}

#[test]
fn platform_chain_at_each_stage_site_and_profile_site() {
    for site in [
        "build",
        "run",
        "languages.demo.build",
        "languages.demo.run",
        "commands.linux",
        "config.build",
        "config.run",
    ] {
        let problem = site.starts_with("config.");
        let kind = if problem {
            LayerKind::Problem
        } else {
            LayerKind::Workspace
        };
        let prefix = if problem {
            "[problem]\nid = 'local:p'\n"
        } else {
            ""
        };
        let text = format!(
            "{prefix}[{site}]\nargv = ['generic']\n[{site}.unix]\nargv = ['unix']\n[{site}.linux]\nargv = ['linux']\n[{site}.macos]\nargv = ['macos']\n[{site}.windows]\nargv = ['windows']"
        );
        for (platform, expected, base_line) in [
            (Platform::Linux, "linux", 6),
            (Platform::Macos, "macos", 8),
            (Platform::Windows, "windows", 10),
        ] {
            let result = merged(&[(&text, kind)], platform);
            let mut keys: Vec<_> = site.split('.').collect();
            keys.push("argv");
            let node = result.at(&keys).unwrap();
            assert_eq!(value(node), json!([expected]));
            origin(
                node,
                kind,
                platform,
                base_line + if problem { 2 } else { 0 },
                1,
                &format!("{site}.{expected}.argv"),
                Some(&format!("{site}.{expected}")),
            );
        }
    }
    let text = "[languages.demo.profiles.p.run]\nargv = ['generic']\n[languages.demo.profiles.p.unix.run]\nargv = ['unix']\n[languages.demo.profiles.p.macos.run]\nargv = ['mac']";
    let result = merged(&[(text, LayerKind::Workspace)], Platform::Macos);
    let profile = result.language("demo").unwrap().profile("p").unwrap();
    let node = profile.run().unwrap().argv().unwrap();
    assert_eq!(value(node), json!(["mac"]));
    origin(
        node,
        LayerKind::Workspace,
        Platform::Macos,
        6,
        1,
        "languages.demo.profiles.p.macos.run.argv",
        Some("languages.demo.profiles.p.macos"),
    );
}

#[test]
fn builtin_reset_allows_a_fresh_python_build_and_another_reset() {
    let user = layer(
        "[languages.python.build]\nargv = ['py-compile', '{sources}']",
        LayerKind::DefaultUser,
        Platform::Linux,
    );
    let result = compose(std::slice::from_ref(&user), Platform::Linux);
    assert!(result.diagnostics.is_empty());
    let build = result.language("python").unwrap().build().unwrap();
    assert_eq!(
        value(build.node),
        json!({"argv":["py-compile","{sources}"]})
    );
    assert!(build.node.reset_occurred);
    assert_eq!(
        build.node.events.iter().map(|e| e.kind).collect::<Vec<_>>(),
        [EventKind::Reset, EventKind::Replace]
    );
    assert_eq!(
        build.node.events[1].removed[0].provenance,
        Provenance::BuiltIn
    );
    let workspace = layer(
        "[languages.python]\nbuild = false\n[build]\ndeadline_s = 5",
        LayerKind::Workspace,
        Platform::Linux,
    );
    let result = compose(&[user, workspace], Platform::Linux);
    let build = result.language("python").unwrap().build().unwrap();
    assert_eq!(value(build.node), json!(false));
    assert!(build.node.reset_occurred);
    // Row 2 remains available for C1c to report as ignored for a build-free target.
    assert_eq!(value(result.build().unwrap().node), json!({"deadline_s":5}));
}

#[test]
fn reset_history_captures_the_table_after_generic_contributions() {
    let result = merged(
        &[
            (
                "[languages.demo.build]\nargv = ['old']\nenv = { A = 'user', B = 'user' }",
                LayerKind::DefaultUser,
            ),
            (
                "[languages.demo.build]\nenv = { A = 'local' }\n[languages.demo.windows]\nbuild = false",
                LayerKind::Workspace,
            ),
        ],
        Platform::Windows,
    );
    let build = result.language("demo").unwrap().build().unwrap();
    assert_eq!(value(build.node), json!(false));
    assert!(build.node.reset_occurred);
    assert_eq!(
        build
            .node
            .events
            .iter()
            .map(|event| event.kind)
            .collect::<Vec<_>>(),
        [EventKind::Set, EventKind::Set, EventKind::Reset]
    );
    let reset = build.node.events.last().unwrap();
    assert_eq!(
        plain(&reset.removed[0].value),
        json!({"argv":["old"],"env":{"a":"local","b":"user"}})
    );
    let history = &result.history[&path(&["languages", "demo", "build", "env", "a"])];
    assert_eq!(
        history.iter().map(|event| event.kind).collect::<Vec<_>>(),
        [EventKind::Set, EventKind::Replace, EventKind::Discarded]
    );
    assert_eq!(plain(&history[1].removed[0].value), json!("user"));
    assert_eq!(plain(&history[2].removed[0].value), json!("local"));
}

fn env_entries(result: &MergedConfig) -> Vec<(String, Value)> {
    result
        .run()
        .unwrap()
        .env_entries()
        .map(|(name, node)| (name.to_owned(), value(node)))
        .collect()
}

#[test]
fn env_names_keep_the_spelling_of_the_winning_source() {
    let texts = [
        ("[run]\nenv = { MY_FLAG = '1' }", LayerKind::DefaultUser),
        ("[run]\nenv = { my_flag = '2' }", LayerKind::Workspace),
    ];
    let result = merged(&texts, Platform::Windows);
    let env = result.run().unwrap().env().unwrap();
    assert_eq!(env.len(), 1);
    assert_eq!(value(&env["my_flag"]), json!("2"));
    assert_eq!(env_entries(&result), [("my_flag".to_owned(), json!("2"))]);
    let result = merged(
        &[
            ("[run]\nenv = { my_flag = '1' }", LayerKind::DefaultUser),
            ("[run]\nenv = { MY_FLAG = false }", LayerKind::Workspace),
            ("[run]\nenv = { My_Flag = '3' }", LayerKind::Workspace),
        ],
        Platform::Windows,
    );
    assert_eq!(env_entries(&result), [("My_Flag".to_owned(), json!("3"))]);
    for platform in [Platform::Linux, Platform::Macos] {
        let result = merged(&texts, platform);
        assert_eq!(
            env_entries(&result),
            [
                ("MY_FLAG".to_owned(), json!("1")),
                ("my_flag".to_owned(), json!("2"))
            ]
        );
    }
}

#[test]
fn overlay_only_tables_are_absent_where_no_overlay_applies() {
    let user = "[languages.python.build.windows]\nargv = ['py', '-m', 'py_compile', '{sources}']";
    for text in [
        user,
        "[languages.python.windows.build]\nargv = ['py', '-m', 'py_compile', '{sources}']",
    ] {
        let result = merged(&[(text, LayerKind::DefaultUser)], Platform::Linux);
        let build = result.language("python").unwrap().build().unwrap();
        assert_eq!(value(build.node), json!(false), "{text}");
        assert_eq!(build.node.provenance, Provenance::BuiltIn);
        assert_eq!(
            build.node.events.iter().map(|e| e.kind).collect::<Vec<_>>(),
            [EventKind::Reset]
        );
        assert!(!result.history.keys().any(|key| key.starts_with(&path(&[
            "languages",
            "python",
            "build",
            "argv"
        ]))));
    }
    let result = merged(&[(user, LayerKind::DefaultUser)], Platform::Windows);
    let build = result.language("python").unwrap().build().unwrap();
    assert_eq!(
        value(build.node),
        json!({"argv":["py","-m","py_compile","{sources}"]})
    );
    assert!(build.node.reset_occurred);
    assert_eq!(
        build.argv().unwrap().overlay.as_deref(),
        Some("languages.python.build.windows")
    );
    assert_eq!(
        build.node.events.iter().map(|e| e.kind).collect::<Vec<_>>(),
        [EventKind::Reset, EventKind::Replace, EventKind::Set]
    );
    // An explicitly written empty table is present.
    for text in ["[languages.python.build]", "[languages.python]\nbuild = {}"] {
        let result = merged(&[(text, LayerKind::DefaultUser)], Platform::Linux);
        let build = result.language("python").unwrap().build().unwrap();
        assert_eq!(value(build.node), json!({}), "{text}");
        assert!(build.node.reset_occurred);
        assert_eq!(
            build.node.events.iter().map(|e| e.kind).collect::<Vec<_>>(),
            [EventKind::Reset, EventKind::Replace]
        );
    }
    let text = "[commands.x.windows]\nargv = ['w']";
    let result = merged(&[(text, LayerKind::Workspace)], Platform::Linux);
    assert!(result.command("x").is_none());
    assert!(result.commands().is_none());
    let result = merged(&[(text, LayerKind::Workspace)], Platform::Windows);
    assert_eq!(
        value(result.command("x").unwrap().node),
        json!({"argv":["w"]})
    );
}

#[test]
fn rejected_keys_supply_no_effective_values() {
    let user = layer(
        "[config.run]\nargv = ['x']",
        LayerKind::DefaultUser,
        Platform::Linux,
    );
    assert_eq!(
        user.diagnostics
            .iter()
            .map(|d| (d.origin.key_path.as_str(), d.message.as_str()))
            .collect::<Vec<_>>(),
        [("config", "unknown configuration key")]
    );
    let problem = layer(
        "[problem]\nid = 'local:p'\n[config.run]\ndeadline_s = 7",
        LayerKind::Problem,
        Platform::Linux,
    );
    let result = compose(&[user.clone(), problem], Platform::Linux);
    assert_eq!(result.diagnostics, user.diagnostics);
    let row = result.problem().unwrap();
    let run = row.run().unwrap();
    assert_eq!(value(run.node), json!({"deadline_s":7}));
    assert!(run.argv().is_none());
    assert!(row.node.markers.is_empty());
    assert!(run.node.markers.is_empty());
    assert_eq!(result.markers.len(), 1);
    assert!(result.markers[0].rejected_key && result.markers[0].applies);
    assert_eq!(result.markers[0].path, path(&["config"]));
    assert_eq!(result.root.markers, result.markers);
    assert!(compose(&[user], Platform::Linux).problem().is_none());

    let layers = [
        layer(
            "[hooks]\nenabled = false",
            LayerKind::DefaultUser,
            Platform::Linux,
        ),
        layer(
            "[hooks]\nenabled = true\ntrusted_workspaces = ['/w']\non_switch = ['c']",
            LayerKind::Workspace,
            Platform::Linux,
        ),
    ];
    let result = compose(&layers, Platform::Linux);
    assert_eq!(result.diagnostics.len(), 2);
    let hooks = result.hooks().unwrap();
    let enabled = hooks.enabled().unwrap();
    assert_eq!(value(enabled), json!(false));
    origin(
        enabled,
        LayerKind::DefaultUser,
        Platform::Linux,
        2,
        1,
        "hooks.enabled",
        None,
    );
    assert!(enabled.markers.is_empty());
    assert!(hooks.trusted_workspaces().is_none());
    assert_eq!(value(hooks.on_switch().unwrap()), json!(["c"]));
    assert_eq!(result.markers.len(), 2);
    assert!(
        result
            .markers
            .iter()
            .all(|m| m.rejected_key && m.layer == LayerKind::Workspace)
    );
    let result = compose(&layers[1..], Platform::Linux);
    assert!(result.hooks().unwrap().enabled().is_none());
}

#[test]
fn merge_from_composes_rows_with_or_without_resets() {
    let result = merged(
        &[(
            "[build]\ndeadline_s = 9\nenv = { A = '1' }\n[languages.python.build]\nargv = ['py']",
            LayerKind::Workspace,
        )],
        Platform::Linux,
    );
    let global = result.build().unwrap().node.clone();
    let row3 = result
        .language("python")
        .unwrap()
        .build()
        .unwrap()
        .node
        .clone();
    assert!(row3.reset_occurred);
    assert_eq!(row3.kind(), NodeKind::Build);
    let build = path(&["build"]);

    let mut preserved = global.clone();
    let discarded = preserved.merge_from(row3.clone(), &build, ResetMode::Preserve);
    assert_eq!(
        value(&preserved),
        json!({"deadline_s":9,"env":{"A":"1"},"argv":["py"]})
    );
    assert!(preserved.reset_occurred);
    assert!(discarded.is_empty());

    let mut reset = global;
    let discarded = reset.merge_from(row3, &build, ResetMode::Discard);
    assert_eq!(value(&reset), json!({"argv":["py"]}));
    for key in [&["build", "deadline_s"][..], &["build", "env", "A"][..]] {
        assert_eq!(discarded[&path(key)][0].kind, EventKind::Discarded);
    }

    // A CLI value composes like any higher row.
    let cli = Provenance::Cli {
        flag: "--deadline".into(),
    };
    let leaf = EffectiveNode::new(
        "deadline_s",
        NodeKind::Other,
        EffectiveValue::Scalar(RawValue::Integer(3)),
        Some(TypedValue::Milliseconds(3000)),
        cli.clone(),
        preserved.origin.clone(),
    );
    let higher = EffectiveNode::new(
        "build",
        NodeKind::Stage,
        EffectiveValue::Table(BTreeMap::from([("deadline_s".to_owned(), leaf)])),
        None,
        cli.clone(),
        preserved.origin.clone(),
    );
    preserved.merge_from(higher, &build, ResetMode::Preserve);
    let deadline = preserved.get("deadline_s").unwrap();
    assert_eq!(value(deadline), json!(3));
    assert_eq!(deadline.provenance, cli);
    assert_eq!(value(preserved.get("argv").unwrap()), json!(["py"]));
}
