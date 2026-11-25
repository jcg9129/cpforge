use super::*;

fn classify(text: &str) -> Record<Catalog, Diagnostic> {
    classify_catalog(text.as_bytes(), Path::new("tests.toml"))
}

fn entry(id: &str, input: &str, answer: Option<&str>) -> String {
    format!(
        "[[test]]\nid = '{id}'\ninput = '{input}'\n{}origin = 'custom'\nenabled = true\n",
        answer.map_or(String::new(), |answer| format!("answer = '{answer}'\n"))
    )
}

#[test]
fn schema_first_and_all_catalog_field_rules() {
    let valid = entry("one", "tests/one.in", Some("tests/one.ans"));
    assert!(matches!(classify(&valid), Record::Valid(_)));
    assert!(matches!(
        classify(&format!("schema = 2\n{valid}bad = []\n")),
        Record::TooNew { schema: 2, .. }
    ));
    for text in [
        "schema = 0",
        "schema = '1'",
        "schema = [",
        "test = []",
        "test = 'one'",
    ] {
        assert!(matches!(classify(text), Record::Malformed(_)), "{text}");
    }
    for (from, to) in [
        ("id = 'one'", "id = 'bad id'"),
        ("input = 'tests/one.in'", "input = '/tests/one.in'"),
        ("input = 'tests/one.in'", "input = 'other/one.in'"),
        ("input = 'tests/one.in'", "input = 'tests/../one.in'"),
        ("input = 'tests/one.in'", "input = 'tests/nested/one.in'"),
        ("input = 'tests/one.in'", "input = 'tests\\one.in'"),
        ("input = 'tests/one.in'", "input = 'tests/..'"),
        ("input = 'tests/one.in'", "input = 'tests/CON.in'"),
        ("answer = 'tests/one.ans'", "answer = 'tests/ONE.IN'"),
        ("answer = 'tests/one.ans'", "answer = false"),
        ("origin = 'custom'", "origin = 'unknown'"),
        ("enabled = true", "enabled = 'yes'"),
        ("id = 'one'\n", ""),
        ("input = 'tests/one.in'\n", ""),
        ("origin = 'custom'\n", ""),
        ("enabled = true\n", ""),
    ] {
        let text = valid.replace(from, to);
        assert!(matches!(classify(&text), Record::Malformed(_)), "{text}");
    }
    for other in [
        entry("ONE", "tests/two.in", None),
        entry("two", "tests/ONE.IN", None),
        entry("two", "tests/ONE.ANS", None),
        entry("two", "tests/two.in", Some("tests/ONE.IN")),
    ] {
        assert!(matches!(
            classify(&(valid.clone() + &other)),
            Record::Malformed(_)
        ));
    }
    // Paths compare using Unicode canonical caseless keys, too.
    let text = entry("one", "tests/é.in", None) + &entry("two", "tests/e\u{301}.in", None);
    assert!(matches!(classify(&text), Record::Malformed(_)));
    for origin in ["sample", "custom", "stress", "imported"] {
        assert!(matches!(
            classify(&valid.replace("custom", origin)),
            Record::Valid(_)
        ));
    }
    assert!(
        matches!(classify("# empty catalog\n"), Record::Valid(Catalog { entries }) if entries.is_empty())
    );
    assert!(matches!(
        classify_catalog(
            &vec![b' '; storage::RECORD_SIZE_LIMIT + 1],
            Path::new("tests.toml")
        ),
        Record::Malformed(storage::MalformedRecord::TooLarge { .. })
    ));
    assert!(matches!(
        classify_catalog(b"\xff", Path::new("tests.toml")),
        Record::Malformed(storage::MalformedRecord::Encoding(_))
    ));
}

#[test]
fn locations_point_to_original_values_and_unknown_keys_are_allowed() {
    let text = format!(
        "# header\nunknown = 123\n{}extra = 'keep'\n",
        entry("one", "tests/one.in", None)
    );
    let Record::Valid(catalog) = classify(&text) else {
        panic!("expected valid catalog")
    };
    let at = &catalog.entries[0].origin_location;
    assert_eq!(at.file, Path::new("tests.toml"));
    assert_eq!(at.key_path, "test[0].id");
    assert_eq!(at.line, Some(4));
    assert_eq!(at.column, Some(6));
    let Record::Malformed(storage::MalformedRecord::Fields(error)) =
        classify(&text.replace("enabled = true", "enabled = 1"))
    else {
        panic!("expected field diagnostic")
    };
    assert_eq!(error.location.key_path, "test[0].enabled");
    assert_eq!(error.location.line, Some(7));
}

#[test]
fn reserved_check_accepts_any_value_and_records_its_location() {
    for value in ["{ kind = 'checker' }", "'x'", "false", "42", "[]"] {
        let text = format!("{}check = {value}\n", entry("one", "tests/one.in", None));
        let Record::Valid(catalog) = classify(&text) else {
            panic!("reserved key is not a catalog error")
        };
        let at = catalog.entries[0].check.as_ref().unwrap();
        assert_eq!(at.key_path, "test[0].check");
        assert_eq!(at.file, Path::new("tests.toml"));
        assert!(at.line.is_some() && at.column.is_some());
    }
    let Record::Valid(catalog) = classify(&entry("one", "tests/one.in", None)) else {
        panic!("valid")
    };
    assert!(catalog.entries[0].check.is_none());
}
