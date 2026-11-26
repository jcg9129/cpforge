use super::*;

#[path = "../tests/support/local_days.rs"]
mod cases;

fn check(zone: &TimeZone, stored: &str, range: &str, expected: bool) {
    let document: toml_edit::DocumentMut = format!("[problem]\nsolved_at = '{stored}'\n")
        .parse()
        .unwrap();
    let filters = Filters {
        solved: Some(range.into()),
        ..Filters::default()
    };
    assert_eq!(
        matches(&filters, document.get("problem"), zone),
        expected,
        "{zone:?}: {stored} in {range}"
    );
}

#[test]
fn utc_ranges_are_inclusive_and_open_sides_are_unbounded() {
    let zone = TimeZone::posix("UTC0").unwrap();
    for (range, stored, expected) in [
        ("2026-11-24", "2026-11-24T00:00:00.000Z", true),
        ("2026-11-24", "2026-11-24T23:59:59.999Z", true),
        ("2026-11-24", "2026-11-25T00:00:00.000Z", false),
        ("..2026-11-24", "1970-01-01", true),
        ("..2026-11-24", "2026-11-24T23:59:59.999Z", true),
        ("..2026-11-24", "2026-11-25T00:00:00.000Z", false),
        ("2026-11-24..", "2026-11-23T23:59:59.999Z", false),
        ("2026-11-24..", "2026-11-24T00:00:00.000Z", true),
        ("2026-11-24..", "9999-12-31", true),
        ("2026-11-20..2026-11-24", "2026-11-19T23:59:59.999Z", false),
        ("2026-11-20..2026-11-24", "2026-11-20T00:00:00.000Z", true),
        ("2026-11-20..2026-11-24", "2026-11-24T23:59:59.999Z", true),
        ("2026-11-20..2026-11-24", "2026-11-25T00:00:00.000Z", false),
    ] {
        check(&zone, stored, range, expected);
    }
}

#[test]
fn posix_local_days_gaps_folds_and_domain_edges() {
    for &(rule, stored, expected) in cases::POSIX_CASES {
        let zone = TimeZone::posix(rule).unwrap();
        assert_eq!(local_date(stored, &zone).unwrap().to_string(), expected);
        check(&zone, stored, expected, true);
    }
    let zone = TimeZone::posix("<-12>12").unwrap();
    check(&zone, "1970-01-01T00:00:00.000Z", "..1970-01-01", true);
    check(&zone, "1970-01-01T00:00:00.000Z", "1970-01-01", false);
    let midnight = TimeZone::posix(cases::MIDNIGHT).unwrap();
    assert_eq!(
        midnight
            .to_datetime("2026-09-06T03:00:00Z".parse().unwrap())
            .hour(),
        1
    );
}

#[test]
fn every_second_of_short_and_long_days_has_one_local_date() {
    let zone = TimeZone::posix(cases::EASTERN).unwrap();
    for (start, end, day, hours) in [
        (
            "2026-03-08T05:00:00Z",
            "2026-03-09T04:00:00Z",
            "2026-03-08",
            23,
        ),
        (
            "2026-11-01T04:00:00Z",
            "2026-11-02T05:00:00Z",
            "2026-11-01",
            25,
        ),
    ] {
        let start: Timestamp = start.parse().unwrap();
        let end: Timestamp = end.parse().unwrap();
        assert_eq!(end.as_second() - start.as_second(), hours * 3600);
        let previous = if hours == 23 {
            "..2026-03-07"
        } else {
            "..2026-10-31"
        };
        let next = if hours == 23 {
            "2026-03-09.."
        } else {
            "2026-11-02.."
        };
        // Every second, plus millisecond precision at the exclusive bounds.
        for second in start.as_second()..end.as_second() {
            let instant = Timestamp::from_second(second).unwrap();
            let date = zone.to_datetime(instant).date();
            assert!(in_range(date, day));
            assert!(!in_range(date, previous));
            assert!(!in_range(date, next));
        }
        for (millis, expected) in [
            (start.as_millisecond() - 1, false),
            (start.as_millisecond(), true),
            (end.as_millisecond() - 1, true),
            (end.as_millisecond(), false),
        ] {
            let date = zone
                .to_datetime(Timestamp::from_millisecond(millis).unwrap())
                .date();
            assert_eq!(in_range(date, day), expected);
        }
    }
}

#[test]
fn tzif_history_and_footer_use_only_pinned_repository_data() {
    for &(name, stored, expected) in cases::TZIF_CASES {
        let bytes: &[u8] = match name {
            "America/Sao_Paulo" => include_bytes!("../tests/fixtures/timezones/America/Sao_Paulo"),
            "Europe/Berlin" => include_bytes!("../tests/fixtures/timezones/Europe/Berlin"),
            _ => unreachable!(),
        };
        let zone = TimeZone::tzif(name, bytes).unwrap();
        assert_eq!(local_date(stored, &zone).unwrap().to_string(), expected);
        check(&zone, stored, expected, true);
        check(&zone, "2026-11-24", "2026-11-24", true);
    }
}

#[test]
fn invalid_and_absent_dates_never_match() {
    let zone = TimeZone::posix("UTC0").unwrap();
    for fields in [
        "",
        "solved_at = 'invalid'",
        "solved_at = 2026-11-24T10:00:00Z",
        "solved_at = '9999-12-30T22:00:01.000Z'",
        "solved_at = '1969-12-31'",
    ] {
        let document: toml_edit::DocumentMut = format!("[problem]\n{fields}\n").parse().unwrap();
        let filters = Filters {
            solved: Some("1970-01-01..".into()),
            ..Filters::default()
        };
        assert!(
            !matches(&filters, document.get("problem"), &zone),
            "{fields}"
        );
    }
}

#[test]
fn windows_decision_ignores_standard_bias_without_transition_dates() {
    // Construct the relevant Windows fields on every OS; no host API or DB.
    for (disabled, daylight_month, bias, standard_bias, expected) in [
        (true, 0, -60, 0, Some(3600)),
        (true, 3, -60, 0, None),
        (false, 3, -60, 0, None),
        (true, 0, 300, 0, Some(-18000)),
        (true, 0, -60, -30, Some(3600)),
    ] {
        let info = WindowsTimeZone {
            disabled,
            daylight_month,
            bias,
            _standard_bias: standard_bias,
        };
        assert_eq!(windows_fixed_offset(info), expected);
    }
}
