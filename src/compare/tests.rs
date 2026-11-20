use super::*;
use crate::config::{
    self, Combine, Environment, LayerKind, LayerSource, LayerState, Platform, RelativeBase,
    TypedValue,
};

fn settings(abs: Option<f64>, rel: Option<f64>, ulps: Option<u64>) -> FloatComparator {
    FloatComparator {
        abs,
        rel,
        ulps,
        rel_base: RelativeBase::Answer,
        combine: Combine::Either,
        int_exact: true,
    }
}

fn f() -> Comparator {
    Comparator::Float(settings(Some(1e-6), Some(1e-6), None))
}

fn loose() -> Comparator {
    let Comparator::Float(mut settings) = f() else {
        unreachable!()
    };
    settings.int_exact = false;
    Comparator::Float(settings)
}

fn abs(bound: f64) -> Comparator {
    Comparator::Float(settings(Some(bound), None, None))
}

fn rel(bound: f64, base: RelativeBase) -> Comparator {
    Comparator::Float(FloatComparator {
        rel_base: base,
        ..settings(None, Some(bound), None)
    })
}

fn ulps(bound: u64) -> Comparator {
    Comparator::Float(settings(None, None, Some(bound)))
}

fn combined(combine: Combine) -> Comparator {
    Comparator::Float(FloatComparator {
        combine,
        ..settings(Some(1e-3), Some(1e-6), None)
    })
}

#[derive(Clone, Copy, Debug)]
enum Expected {
    Pass,
    Fail(&'static str),
    Byte(u64),
    Token(u64),
    Count(u64, u64),
    Ulp(u128),
    Invalid,
    Unchecked,
}

#[derive(Debug)]
enum Case {
    Compare {
        comparator: Comparator,
        output: Vec<u8>,
        answer: Option<Vec<u8>>,
        complete: bool,
        expected: Expected,
    },
    Config {
        text: &'static str,
        accepted: bool,
    },
    Excerpt {
        bytes: Vec<u8>,
        rendered: String,
        truncated: bool,
    },
}

fn case(
    comparator: Comparator,
    output: impl AsRef<[u8]>,
    answer: impl AsRef<[u8]>,
    expected: Expected,
) -> Case {
    Case::Compare {
        comparator,
        output: output.as_ref().to_vec(),
        answer: Some(answer.as_ref().to_vec()),
        complete: true,
        expected,
    }
}

fn kind(diag: &Diag) -> String {
    serde_json::to_value(diag).unwrap()["kind"]
        .as_str()
        .unwrap()
        .into()
}

fn assert_outcome(name: &str, outcome: CheckOutcome, expected: Expected) {
    match (expected, &outcome) {
        (Expected::Pass, CheckOutcome::Passed) | (Expected::Unchecked, CheckOutcome::Unchecked) => {
        }
        (Expected::Fail(expected), CheckOutcome::Failed(reason)) => {
            assert_eq!(kind(reason), expected, "{name}: {outcome}");
        }
        (Expected::Byte(expected), CheckOutcome::Failed(reason)) => {
            assert!(
                matches!(**reason, Diag::ByteMismatch { offset, .. } if offset == expected),
                "{name}: {outcome}"
            );
        }
        (Expected::Token(expected), CheckOutcome::Failed(reason)) => {
            assert!(
                matches!(**reason, Diag::TokenMismatch { index, .. } if index == expected),
                "{name}: {outcome}"
            );
        }
        (Expected::Count(expected_o, expected_a), CheckOutcome::Failed(reason)) => {
            assert!(
                matches!(**reason, Diag::CountMismatch { unit: Unit::Tokens, o, a, .. } if (o, a) == (expected_o, expected_a)),
                "{name}: {outcome}"
            );
        }
        (Expected::Ulp(expected), CheckOutcome::Failed(reason)) => {
            assert!(
                matches!(**reason, Diag::UlpExcess { dist, .. } if dist == expected),
                "{name}: {outcome}"
            );
        }
        (Expected::Invalid, CheckOutcome::CheckerFailed(reason)) => {
            assert_eq!(kind(reason), "invalid_answer", "{name}: {outcome}");
        }
        _ => panic!("{name}: expected {expected:?}, got {outcome:?}"),
    }
}

/// Every row in compare-spec v4 §7, including configuration and excerpt rows.
#[test]
fn specification_table() {
    use Comparator::{Exact, Lines, Tokens};
    use Expected::{Byte, Count, Fail, Invalid, Pass, Token, Ulp, Unchecked};
    use RelativeBase::{Answer, Max, Max1};

    let huge = format!("1{}", "0".repeat(400));
    let huge_other = format!("1{}1", "0".repeat(399));
    let rows = [
        ("01 exact CRLF", case(Exact, b"1\n", b"1\r\n", Byte(1))),
        (
            "02 token whitespace",
            case(Tokens, b"1 2\r\n", b"1\n2", Pass),
        ),
        ("03 empty whitespace", case(Tokens, b"", b" \n", Pass)),
        ("04 VT", case(Tokens, b"a\x0bb", b"a b", Pass)),
        ("05 FF", case(Tokens, b"a\x0cb", b"a b", Pass)),
        (
            "06 NBSP content",
            case(Tokens, b"a\xc2\xa0b", b"a b", Fail("token_mismatch")),
        ),
        (
            "07 NUL content",
            case(Tokens, b"a\0b", b"ab", Fail("token_mismatch")),
        ),
        (
            "08 invalid UTF-8 equal",
            case(Tokens, b"\xff", b"\xff", Pass),
        ),
        (
            "09 invalid UTF-8 unequal",
            case(Tokens, b"\xff", b"\xfe", Fail("token_mismatch")),
        ),
        (
            "10 tokens leading zero",
            case(Tokens, b"007", b"7", Fail("token_mismatch")),
        ),
        (
            "11 pair before count",
            case(Tokens, b"X extra", b"Y", Token(1)),
        ),
        (
            "12 extra output",
            case(Tokens, b"Y extra", b"Y", Count(2, 1)),
        ),
        (
            "13 extra answer",
            case(Tokens, b"Y", b"Y extra", Count(1, 2)),
        ),
        (
            "14 lines trailing blanks",
            case(Lines, b"a b  \n\n\n", b"a b", Pass),
        ),
        ("15 lines CRLF", case(Lines, b"a\r\n", b"a\n", Pass)),
        (
            "16 lines leading space",
            case(Lines, b" a", b"a", Fail("line_mismatch")),
        ),
        (
            "17 lines leading blank",
            case(Lines, b"\na", b"a", Fail("line_mismatch")),
        ),
        (
            "18 lines interior blank",
            case(Lines, b"a\n\nb", b"a\nb", Fail("line_mismatch")),
        ),
        (
            "19 lines interior space",
            case(Lines, b"a  b", b"a b", Fail("line_mismatch")),
        ),
        (
            "20 lines interior CR",
            case(Lines, b"a\rb", b"a\nb", Fail("line_mismatch")),
        ),
        (
            "21 strict integer spelling",
            case(f(), b"5.0", b"5", Fail("class_mismatch")),
        ),
        (
            "22 loose integer spelling",
            case(loose(), b"5.0", b"5", Pass),
        ),
        (
            "23 loose relative equality",
            case(loose(), b"1000001", b"1000000", Pass),
        ),
        (
            "24 loose relative excess",
            case(loose(), b"1000002", b"1000000", Fail("tolerance")),
        ),
        (
            "25 strict integer equality",
            case(f(), b"1000001", b"1000000", Fail("integer_mismatch")),
        ),
        (
            "26 decimal cannot bypass integer",
            case(f(), b"1000000.0", b"1000000", Fail("class_mismatch")),
        ),
        ("27 integer normalisation", case(f(), b"007", b"7", Pass)),
        (
            "28 strict above binary64 precision",
            case(
                f(),
                b"10000000000000000001",
                b"10000000000000000000",
                Fail("integer_mismatch"),
            ),
        ),
        (
            "29 loose binary64 conflation",
            case(
                loose(),
                b"10000000000000000001",
                b"10000000000000000000",
                Pass,
            ),
        ),
        (
            "30 decimal tolerance",
            case(f(), b"2.5000001", b"2.5", Pass),
        ),
        (
            "31 binary64 boundary",
            case(abs(0.1), b"1.1", b"1.0", Fail("tolerance")),
        ),
        (
            "32 exact boundary",
            case(abs(0.125), b"1.125", b"1.0", Pass),
        ),
        (
            "33 above exact boundary",
            case(abs(0.125), b"1.1250000000000002", b"1.0", Fail("tolerance")),
        ),
        (
            "34 relative answer base",
            case(rel(0.12, Answer), b"1.125", b"1.0", Fail("tolerance")),
        ),
        (
            "35 relative asymmetry",
            case(rel(0.12, Answer), b"1.0", b"1.125", Pass),
        ),
        (
            "36 relative max base",
            case(rel(0.12, Max), b"1.125", b"1.0", Pass),
        ),
        (
            "37 relative zero base",
            case(rel(1e-6, Answer), b"1e-7", b"0.0", Fail("tolerance")),
        ),
        (
            "38 relative max1 base",
            case(rel(1e-6, Max1), b"1e-7", b"0.0", Pass),
        ),
        (
            "39 both bounds hold",
            case(combined(Combine::Both), b"1000000.0005", b"1000000.0", Pass),
        ),
        (
            "40 both requires relative",
            case(
                combined(Combine::Both),
                b"1.0005",
                b"1.0",
                Fail("tolerance"),
            ),
        ),
        (
            "41 either absolute holds",
            case(combined(Combine::Either), b"1.0005", b"1.0", Pass),
        ),
        (
            "42 scaled evaluation",
            case(rel(3.0, Answer), b"1e308", b"-1e308", Pass),
        ),
        (
            "43 scaled excess",
            case(f(), b"1e308", b"-1e308", Fail("tolerance")),
        ),
        ("44 signed decimal zero", case(f(), b"-0.0", b"0.0", Pass)),
        ("45 signed integer zero", case(f(), b"-0", b"0", Pass)),
        ("46 subnormal", case(f(), b"4.9e-324", b"0.0", Pass)),
        (
            "47 underflow equality",
            case(abs(0.0), b"1e-400", b"2e-400", Pass),
        ),
        (
            "48 same binary64 decimal",
            case(abs(0.0), b"0.1", b"0.10", Pass),
        ),
        (
            "49 rounded decimal integers",
            case(abs(0.0), b"9007199254740993.0", b"9007199254740992.0", Pass),
        ),
        ("50 ULP signed zero", case(ulps(0), b"-0.0", b"0.0", Pass)),
        (
            "51 ULP adjacency",
            case(ulps(1), b"0.30000000000000004", b"0.3", Pass),
        ),
        (
            "52 ULP nonidentity",
            case(ulps(0), b"0.30000000000000004", b"0.3", Fail("ulp_excess")),
        ),
        (
            "53 ULP negative subnormal to zero",
            case(ulps(1), b"-5e-324", b"0.0", Pass),
        ),
        (
            "54 ULP across zero excess",
            case(ulps(1), b"-5e-324", b"5e-324", Ulp(2)),
        ),
        (
            "55 ULP across zero equality",
            case(ulps(2), b"-5e-324", b"5e-324", Pass),
        ),
        (
            "56 ULP negative adjacency",
            case(ulps(1), b"-1.0", b"-0.9999999999999999", Pass),
        ),
        (
            "57 ULP normal subnormal boundary",
            case(
                ulps(1),
                b"2.2250738585072014e-308",
                b"2.225073858507201e-308",
                Pass,
            ),
        ),
        (
            "58 ULP maximum supported bound",
            case(
                ulps(i64::MAX as u64),
                b"1e308",
                b"-1e308",
                Ulp(18_429_743_317_745_373_504),
            ),
        ),
        (
            "59 ULP exact large bound",
            case(ulps(9007199254740993), b"1.0", b"4.000000000000001", Pass),
        ),
        (
            "60 ULP exact large bound excess",
            case(
                ulps(9007199254740992),
                b"1.0",
                b"4.000000000000001",
                Ulp(9_007_199_254_740_993),
            ),
        ),
        (
            "61 output overflow",
            case(f(), b"1e400", b"1.0", Fail("out_of_range")),
        ),
        ("62 answer overflow", case(f(), b"1.0", b"1e400", Invalid)),
        ("63 answer NaN", case(f(), b"nan", b"nan", Invalid)),
        ("64 answer infinity", case(f(), b"1", b"inf", Invalid)),
        (
            "65 output NaN",
            case(f(), b"nan", b"1.0", Fail("class_mismatch")),
        ),
        (
            "66 hexadecimal other",
            case(f(), b"0x10", b"16", Fail("class_mismatch")),
        ),
        ("67 optional fraction", case(f(), b"1.e2", b"100.0", Pass)),
        (
            "68 incomplete exponent class",
            case(f(), b"1e", b"1.0", Fail("class_mismatch")),
        ),
        ("69 other textual equality", case(f(), b"1e", b"1e", Pass)),
        ("70 text equality", case(f(), b"YES", b"YES", Pass)),
        (
            "71 text case",
            case(f(), b"yes", b"YES", Fail("token_mismatch")),
        ),
        (
            "72 long mantissa",
            case(f(), format!("{huge}e-400"), b"1.0", Pass),
        ),
        (
            "73 unbounded exponent digits",
            case(f(), format!("1e-{}1", "0".repeat(500)), b"0.1", Pass),
        ),
        ("74 unbounded equal integers", case(f(), &huge, &huge, Pass)),
        (
            "75 unbounded unequal integers",
            case(f(), huge_other, &huge, Fail("integer_mismatch")),
        ),
        (
            "76 loose integer overflow",
            case(loose(), &huge, &huge, Invalid),
        ),
        (
            "77 unpaired invalid answer",
            case(f(), b"", b"nan", Invalid),
        ),
        (
            "78 invalid answer after mismatch",
            case(f(), b"X", b"Y nan", Invalid),
        ),
        (
            "79 overflowing unscaled difference",
            case(
                rel(1.5, Answer),
                b"1.7976931348623157e308",
                b"-1.7976931348623157e308",
                Fail("tolerance"),
            ),
        ),
        (
            "80 infinite bound",
            case(
                rel(8.0, Answer),
                b"1.7976931348623157e308",
                b"-1.7976931348623157e308",
                Pass,
            ),
        ),
        (
            "81 below scaling threshold",
            case(
                f(),
                b"4.4942328371557893e307",
                b"4.4942328371557893e307",
                Pass,
            ),
        ),
        (
            "82 at scaling threshold",
            case(
                f(),
                b"4.49423283715579e307",
                b"4.4942328371557893e307",
                Pass,
            ),
        ),
        (
            "83 scaled answer underflow",
            case(
                rel(f64::MAX, Answer),
                b"1e308",
                b"5e-324",
                Fail("tolerance"),
            ),
        ),
        (
            "84 scaled absolute underflow",
            case(abs(f64::from_bits(1)), b"1e308", b"0.0", Fail("tolerance")),
        ),
        (
            "85 scaled max1",
            case(rel(1e-6, Max1), b"1e308", b"1e-300", Fail("tolerance")),
        ),
        (
            "86 config negative underflow",
            Case::Config {
                text: "float = { abs = -1e-400 }",
                accepted: true,
            },
        ),
        (
            "87 config negative zero",
            Case::Config {
                text: "float = { abs = -0.0 }",
                accepted: true,
            },
        ),
        (
            "88 config multiple selectors",
            Case::Config {
                text: "float_abs = 1e-6, float_rel = 1e-6",
                accepted: false,
            },
        ),
        ("89 complete match", case(Tokens, b"match", b"match", Pass)),
        (
            "90 incomplete match",
            Case::Compare {
                comparator: Tokens,
                output: b"match".to_vec(),
                answer: Some(b"match".to_vec()),
                complete: false,
                expected: Fail("incomplete_output"),
            },
        ),
        (
            "91 incomplete mismatch",
            Case::Compare {
                comparator: Tokens,
                output: b"X".to_vec(),
                answer: Some(b"Y".to_vec()),
                complete: false,
                expected: Fail("token_mismatch"),
            },
        ),
        (
            "92 absent answer",
            Case::Compare {
                comparator: f(),
                output: b"anything".to_vec(),
                answer: None,
                complete: false,
                expected: Unchecked,
            },
        ),
        (
            "93 present empty answer",
            case(Tokens, b"anything", b"", Fail("count_mismatch")),
        ),
        (
            "94 config modifier without bound",
            Case::Config {
                text: "float = { rel_base = 'max', abs = 1 }",
                accepted: false,
            },
        ),
        (
            "95 config negative bound",
            Case::Config {
                text: "float = { abs = -1e-6 }",
                accepted: false,
            },
        ),
        (
            "96 config positive underflow",
            Case::Config {
                text: "float = { abs = 1e-400 }",
                accepted: true,
            },
        ),
        (
            "97 config ULP excludes bound",
            Case::Config {
                text: "float = { ulps = 1, abs = 1e-6 }",
                accepted: false,
            },
        ),
        (
            "98 config combine without both bounds",
            Case::Config {
                text: "float = { combine = 'either', abs = 1e-6 }",
                accepted: false,
            },
        ),
        (
            "99a excerpt 64 bytes",
            Case::Excerpt {
                bytes: vec![b'a'; 64],
                rendered: "a".repeat(64),
                truncated: false,
            },
        ),
        (
            "99b excerpt 65 bytes",
            Case::Excerpt {
                bytes: vec![b'a'; 65],
                rendered: "a".repeat(64),
                truncated: true,
            },
        ),
        (
            "100a literal escape",
            Case::Excerpt {
                bytes: br"\xFF".to_vec(),
                rendered: r"\\xFF".into(),
                truncated: false,
            },
        ),
        (
            "100b escaped byte",
            Case::Excerpt {
                bytes: vec![0xff],
                rendered: r"\xFF".into(),
                truncated: false,
            },
        ),
    ];
    for (name, row) in rows {
        match row {
            Case::Compare {
                comparator,
                output,
                answer,
                complete,
                expected,
            } => {
                assert_outcome(
                    name,
                    check(&comparator, &output, answer.as_deref(), complete),
                    expected,
                );
            }
            Case::Config { text, accepted } => {
                let document = format!("compare = {{ {text} }}");
                let layer = config::parse(
                    document.as_bytes(),
                    &LayerSource {
                        kind: LayerKind::Workspace,
                        file: "compare-table.toml".into(),
                    },
                    &Environment::default(),
                    Platform::Linux,
                );
                assert_eq!(
                    layer.diagnostics.is_empty(),
                    accepted,
                    "{name}: {:?}",
                    layer.diagnostics
                );
                if accepted {
                    let LayerState::Loaded(root) = layer.state else {
                        panic!("{name}: missing root")
                    };
                    let Some(TypedValue::Compare(Comparator::Float(settings))) =
                        &root.get("compare").unwrap().typed
                    else {
                        panic!("{name}: missing typed comparator")
                    };
                    assert_eq!(settings.abs.unwrap().to_bits(), 0.0_f64.to_bits(), "{name}");
                }
            }
            Case::Excerpt {
                bytes,
                rendered,
                truncated,
            } => {
                let excerpt = Excerpt::new(&bytes);
                assert_eq!(excerpt.rendered(), rendered, "{name}");
                assert_eq!(excerpt.truncated(), truncated, "{name}");
            }
        }
    }
}

#[test]
fn completeness_and_answer_presence_for_every_comparator() {
    for comparator in [
        Comparator::Exact,
        Comparator::Tokens,
        Comparator::Lines,
        f(),
    ] {
        for complete in [false, true] {
            assert_eq!(
                check(&comparator, b"anything", None, complete),
                CheckOutcome::Unchecked
            );
            assert_eq!(
                check(&comparator, b"nan", None, complete),
                CheckOutcome::Unchecked
            );
            assert_outcome(
                "empty present",
                check(&comparator, b"", Some(b""), complete),
                if complete {
                    Expected::Pass
                } else {
                    Expected::Fail("incomplete_output")
                },
            );
            assert_outcome(
                "match",
                check(&comparator, b"a", Some(b"a"), complete),
                if complete {
                    Expected::Pass
                } else {
                    Expected::Fail("incomplete_output")
                },
            );
            let outcome = check(&comparator, b"a", Some(b"b"), complete);
            assert!(
                matches!(outcome, CheckOutcome::Failed(ref reason) if !matches!(**reason, Diag::IncompleteOutput))
            );
            if matches!(comparator, Comparator::Float(_)) {
                assert_outcome(
                    "invalid before completeness",
                    check(&comparator, b"X", Some(b"Y nan"), complete),
                    Expected::Invalid,
                );
            }
        }
    }
}

#[test]
fn whitespace_final_newlines_and_binary_content() {
    for (name, output, answer, token_pass, line_pass, exact_pass) in [
        (
            "all whitespace",
            b"".as_slice(),
            b" \t\n\x0b\x0c\r".as_slice(),
            true,
            true,
            false,
        ),
        ("final newline", b"a\n", b"a", true, true, false),
        ("CRLF", b"a\r\n", b"a\n", true, true, false),
        ("line trim", b"a \t\x0b\x0c\r\n", b"a", true, true, false),
        ("leading VT", b"\x0ba", b"a", true, false, false),
        ("interior VT", b"a\x0bb", b"a b", true, false, false),
        (
            "binary equal",
            b"\0\x80\xff",
            b"\0\x80\xff",
            true,
            true,
            true,
        ),
        (
            "binary unequal",
            b"\0\x80\xff",
            b"\0\x80\xfe",
            false,
            false,
            false,
        ),
        (
            "blank line preserved",
            b"\n\na\n",
            b"\na",
            true,
            false,
            false,
        ),
        (
            "trim blank interior",
            b"a\n \t\x0b\x0c\r\nb\n",
            b"a\n\nb",
            true,
            true,
            false,
        ),
    ] {
        assert_eq!(tokens(output, answer).is_ok(), token_pass, "{name}: tokens");
        assert_eq!(lines(output, answer).is_ok(), line_pass, "{name}: lines");
        assert_eq!(exact(output, answer).is_ok(), exact_pass, "{name}: exact");
    }
}

#[test]
fn first_mismatch_positions_and_total_counts_use_original_bytes() {
    let diag = tokens(b"same\r\n \x0b\tbad later extra", b"same\n\tgood").unwrap_err();
    let Diag::TokenMismatch { index, o, a } = *diag else {
        panic!("expected pair mismatch")
    };
    assert_eq!(index, 2);
    assert_eq!((o.offset, o.line, o.col), (9, 2, 4));
    assert_eq!((a.offset, a.line, a.col), (6, 2, 2));
    assert_eq!(o.excerpt.rendered(), "bad");
    assert_eq!(a.excerpt.rendered(), "good");

    let diag = tokens(b"\xff\xc2\xa0 \tX", b"\xff\xc2\xa0 Y").unwrap_err();
    let Diag::TokenMismatch { o, a, .. } = *diag else {
        panic!("expected binary prefix mismatch")
    };
    assert_eq!((o.offset, o.line, o.col), (5, 1, 6));
    assert_eq!((a.offset, a.line, a.col), (4, 1, 5));

    for (output, answer, o_total, a_total, position) in [
        (
            b"same\r\n \tfirst rest last".as_slice(),
            b"same".as_slice(),
            4,
            1,
            (8, 2, 3),
        ),
        (b"same", b"same\r\n \tfirst rest last", 1, 4, (8, 2, 3)),
    ] {
        let diag = tokens(output, answer).unwrap_err();
        let Diag::CountMismatch {
            unit,
            o,
            a,
            first_extra,
        } = *diag
        else {
            panic!("expected count")
        };
        assert_eq!(unit, Unit::Tokens);
        assert_eq!((o, a), (o_total, a_total));
        assert_eq!(
            (first_extra.offset, first_extra.line, first_extra.col),
            position
        );
        assert_eq!(first_extra.excerpt.rendered(), "first");
    }

    let diag = lines(b"same \r\n\tbad\x0b\nmore\n\n", b"same\ngood").unwrap_err();
    let Diag::LineMismatch { line, o, a } = *diag else {
        panic!("expected line mismatch")
    };
    assert_eq!(line, 2);
    assert_eq!((o.offset, o.line, o.col), (7, 2, 1));
    assert_eq!((a.offset, a.line, a.col), (5, 2, 1));
    assert_eq!(o.excerpt.rendered(), "\\tbad");

    let diag = lines(b"same\n\n\nextra\nlast\n \t\n", b"same\n\n").unwrap_err();
    let Diag::CountMismatch {
        unit,
        o,
        a,
        first_extra,
    } = *diag
    else {
        panic!("expected line count")
    };
    assert_eq!((unit, o, a), (Unit::Lines, 5, 1));
    assert_eq!(
        (first_extra.offset, first_extra.line, first_extra.col),
        (5, 2, 1)
    );
    assert_eq!(first_extra.excerpt.rendered(), "");

    for (output, answer, offset, o, a) in [
        (b"abc".as_slice(), b"ab".as_slice(), 2, Some(b'c'), None),
        (b"ab", b"abc", 2, None, Some(b'c')),
        (b"abc", b"ax", 1, Some(b'b'), Some(b'x')),
        (b"", b"x", 0, None, Some(b'x')),
    ] {
        assert_eq!(
            *exact(output, answer).unwrap_err(),
            Diag::ByteMismatch { offset, o, a }
        );
    }
}

#[test]
fn integer_normalisation_and_numeric_pair_precedence() {
    let huge = "9".repeat(10_000);
    for (output, answer, expected) in [
        ("+000123", "123", Expected::Pass),
        ("-000123", "-123", Expected::Pass),
        ("-000000", "+0", Expected::Pass),
        ("0000", "-0", Expected::Pass),
        ("-1", "+1", Expected::Fail("integer_mismatch")),
        ("2.0", "2", Expected::Fail("class_mismatch")),
        ("2e0", "2", Expected::Fail("class_mismatch")),
        ("1e400", "1", Expected::Fail("class_mismatch")),
        (&huge, "1", Expected::Fail("integer_mismatch")),
        (&huge, "1.0", Expected::Fail("out_of_range")),
        ("2", "2.0", Expected::Pass),
        ("nan(1)", "nan(1)", Expected::Pass),
        ("inf", "nan(1)", Expected::Fail("token_mismatch")),
        ("1", "--1", Expected::Fail("token_mismatch")),
        ("\0", "\0", Expected::Pass),
    ] {
        assert_outcome(
            output,
            check(&f(), output.as_bytes(), Some(answer.as_bytes()), true),
            expected,
        );
    }
    let padded = format!("+{}{huge}", "0".repeat(10_000));
    assert_eq!(
        check(&f(), padded.as_bytes(), Some(huge.as_bytes()), true),
        CheckOutcome::Passed
    );
    assert_outcome(
        "huge loose integer",
        check(&loose(), huge.as_bytes(), Some(b"1"), true),
        Expected::Fail("out_of_range"),
    );
    assert_outcome(
        "loose zero bound still rounds",
        check(
            &Comparator::Float(FloatComparator {
                int_exact: false,
                ..settings(Some(0.0), None, None)
            }),
            b"9007199254740993",
            Some(b"9007199254740992"),
            true,
        ),
        Expected::Pass,
    );
}

#[test]
fn float_pairs_precede_counts_and_extra_output_is_only_counted() {
    let CheckOutcome::Failed(diag) = check(&f(), b"1.0 2.0 extra", Some(b"1.0 3.0"), true) else {
        panic!("expected numeric pair mismatch")
    };
    assert!(matches!(*diag, Diag::Tolerance { index: 2, .. }));
    assert_outcome(
        "unpaired output is counted",
        check(&f(), b"1.0 nan 1e400", Some(b"1.0"), true),
        Expected::Count(3, 1),
    );
    let diag = lines(b"same\n\n", b"same\n\n\nextra\nlast\n \t\n").unwrap_err();
    assert!(matches!(
        *diag,
        Diag::CountMismatch {
            unit: Unit::Lines,
            o: 1,
            a: 5,
            ..
        }
    ));

    let mut strict = settings(None, None, Some(i64::MAX as u64));
    assert_outcome(
        "ULP integer exactness",
        check(&Comparator::Float(strict.clone()), b"2", Some(b"1"), true),
        Expected::Fail("integer_mismatch"),
    );
    assert_outcome(
        "ULP integer spelling",
        check(&Comparator::Float(strict.clone()), b"1.0", Some(b"1"), true),
        Expected::Fail("class_mismatch"),
    );
    strict.int_exact = false;
    strict.ulps = Some(0);
    assert_eq!(
        check(&Comparator::Float(strict), b"1.0", Some(b"1"), true),
        CheckOutcome::Passed
    );
}

#[test]
fn invalid_answers_are_validated_in_full_with_the_first_invalid_position() {
    for spelling in [
        "nan",
        "+NaN",
        "-NAN",
        "inf",
        "+INF",
        "-inf",
        "infinity",
        "+Infinity",
        "-INFINITY",
        "1e400",
        "-1E99999999999999999999999",
    ] {
        let answer = format!("ok\r\n \t{spelling} nan");
        let outcome = check(&f(), b"different", Some(answer.as_bytes()), false);
        let CheckOutcome::CheckerFailed(diag) = outcome else {
            panic!("{spelling}: expected invalid answer")
        };
        let Diag::InvalidAnswer { index, a } = *diag else {
            panic!("expected invalid answer diagnostic")
        };
        assert_eq!(index, 2, "{spelling}");
        assert_eq!((a.offset, a.line, a.col), (6, 2, 3), "{spelling}");
        assert_eq!(a.excerpt.rendered(), spelling);
        assert_outcome(
            spelling,
            check(&f(), spelling.as_bytes(), Some(b"0.0"), true),
            Expected::Fail(if spelling == "1e400" || spelling.starts_with("-1E") {
                "out_of_range"
            } else {
                "class_mismatch"
            }),
        );
    }
    assert_outcome(
        "other non-ASCII bytes",
        check(&f(), b"\xff", Some(b"\xff"), true),
        Expected::Pass,
    );
    assert_outcome(
        "answer non-ASCII suffix is other",
        check(&f(), b"1\xff", Some(b"1\xff"), true),
        Expected::Pass,
    );
    assert_outcome(
        "numeric output, textual answer",
        check(&f(), b"1", Some(b"1\xff"), true),
        Expected::Fail("token_mismatch"),
    );
    assert_outcome(
        "decimal answer, non-ASCII output",
        check(&f(), b"1\xff", Some(b"1.0"), true),
        Expected::Fail("class_mismatch"),
    );
    let huge_exponent = format!("1e{}", "9".repeat(10_000));
    assert_outcome(
        "unbounded overflowing exponent",
        check(&f(), b"X", Some(huge_exponent.as_bytes()), true),
        Expected::Invalid,
    );
    let underflow = format!("1e-{}", "9".repeat(10_000));
    assert_eq!(
        check(&abs(0.0), underflow.as_bytes(), Some(b"0.0"), true),
        CheckOutcome::Passed
    );
}

#[test]
fn tolerance_diagnostics_preserve_scaled_operations_bounds_and_bits() {
    for (spelling, bits, expected_scale) in [
        (
            b"4.4942328371557893e307".as_slice(),
            0x7fcf_ffff_ffff_ffff,
            1.0,
        ),
        (b"4.49423283715579e307", 0x7fd0_0000_0000_0000, 0.25),
    ] {
        let CheckOutcome::Failed(diag) = check(&abs(0.0), spelling, Some(b"0.0"), true) else {
            panic!("expected threshold diagnostic")
        };
        let Diag::Tolerance { o_val, scale, .. } = *diag else {
            panic!("expected tolerance")
        };
        assert_eq!(o_val.value().to_bits(), bits);
        assert_eq!(scale.value(), expected_scale);
    }
    let CheckOutcome::Failed(diag) = check(
        &rel(1.5, RelativeBase::Answer),
        b"1.7976931348623157e308",
        Some(b"-1.7976931348623157e308"),
        true,
    ) else {
        panic!("expected tolerance")
    };
    let Diag::Tolerance {
        o_val,
        a_val,
        scale,
        diff_scaled,
        b_abs_scaled,
        b_rel_scaled,
        diff,
        ..
    } = &*diag
    else {
        panic!("expected tolerance diagnostic")
    };
    assert_eq!(o_val.value(), f64::MAX);
    assert_eq!(a_val.value(), -f64::MAX);
    assert_eq!(scale.value(), 0.25);
    assert_eq!(
        diff_scaled.value(),
        (f64::MAX * 0.25 - -f64::MAX * 0.25).abs()
    );
    assert!(b_abs_scaled.is_none());
    assert!(b_rel_scaled.as_ref().unwrap().0.value().is_finite());
    assert!(!b_rel_scaled.as_ref().unwrap().1);
    assert!(diff.is_none());
    assert_eq!(diag.unscaled_bounds(), (None, None));
    assert!(diag.to_string().contains("difference overflow"));

    let CheckOutcome::Failed(diag) = check(&combined(Combine::Both), b"1.0005", Some(b"1.0"), true)
    else {
        panic!("expected both failure")
    };
    let Diag::Tolerance {
        scale,
        b_abs_scaled: Some((abs_bound, abs_held)),
        b_rel_scaled: Some((rel_bound, rel_held)),
        diff: Some(diff),
        ..
    } = &*diag
    else {
        panic!("missing bound diagnostics")
    };
    assert_eq!(scale.value(), 1.0);
    assert_eq!(abs_bound.value(), 1e-3);
    assert_eq!(rel_bound.value(), 1e-6);
    assert!(*abs_held);
    assert!(!rel_held);
    assert_eq!(diff.value(), 1.0005_f64 - 1.0);
    assert_eq!(
        diag.unscaled_bounds(),
        (Some(abs_bound.clone()), Some(rel_bound.clone()))
    );

    for (comparator, output, answer, expected_abs, expected_rel) in [
        (
            rel(f64::MAX, RelativeBase::Answer),
            b"1e308".as_slice(),
            b"5e-324".as_slice(),
            None,
            Some(0.0),
        ),
        (abs(f64::from_bits(1)), b"1e308", b"0.0", Some(0.0), None),
        (
            rel(1e-6, RelativeBase::Max1),
            b"1e308",
            b"1e-300",
            None,
            Some(1e-6 * 0.25),
        ),
    ] {
        let CheckOutcome::Failed(diag) = check(&comparator, output, Some(answer), true) else {
            panic!("expected scaled failure")
        };
        let Diag::Tolerance {
            scale,
            b_abs_scaled,
            b_rel_scaled,
            ..
        } = *diag
        else {
            panic!("expected tolerance")
        };
        assert_eq!(scale.value(), 0.25);
        assert_eq!(b_abs_scaled.map(|(v, _)| v.value()), expected_abs);
        assert_eq!(b_rel_scaled.map(|(v, _)| v.value()), expected_rel);
    }

    let both = Comparator::Float(FloatComparator {
        combine: Combine::Both,
        ..settings(Some(0.0), Some(8.0), None)
    });
    let CheckOutcome::Failed(diag) = check(
        &both,
        b"1.7976931348623157e308",
        Some(b"-1.7976931348623157e308"),
        true,
    ) else {
        panic!("expected absolute bound to fail")
    };
    let Diag::Tolerance {
        b_rel_scaled: Some((bound, held)),
        ..
    } = &*diag
    else {
        panic!("expected infinite relative bound")
    };
    assert!(bound.value().is_infinite());
    assert_eq!(bound.text(), "inf");
    assert!(*held);
    assert_eq!(diag.unscaled_bounds().1, None);
}

#[test]
fn ulp_distances_are_exact_across_signs_and_large_thresholds() {
    for (output, answer, expected) in [
        (b"-5e-324".as_slice(), b"5e-324".as_slice(), 2),
        (b"1e308", b"-1e308", 18_429_743_317_745_373_504),
        (b"1.0", b"4.000000000000001", 9_007_199_254_740_993),
        (b"-1.0", b"1.0", 9_214_364_837_600_034_816),
    ] {
        let CheckOutcome::Failed(diag) = check(&ulps(0), output, Some(answer), true) else {
            panic!("expected ULP excess")
        };
        let Diag::UlpExcess { dist, ulps, .. } = *diag else {
            panic!("expected ULP diagnostic")
        };
        assert_eq!(dist, expected);
        assert_eq!(ulps, 0);
    }
}

#[test]
fn excerpts_are_bounded_injective_and_use_uppercase_hex() {
    let bytes: Vec<_> = (0..=u8::MAX).collect();
    for &byte in &bytes {
        let rendered = Excerpt::new(&[byte]);
        assert!(rendered.rendered().is_ascii());
        assert!(rendered.rendered().len() <= 4);
        for &other in bytes.iter().filter(|&&other| other != byte) {
            assert_ne!(rendered, Excerpt::new(&[other]));
        }
    }
    assert_eq!(
        Excerpt::new(b"\n\r\t\x0b\x0c\0\x80\xff\\\"").rendered(),
        "\\n\\r\\t\\x0B\\x0C\\x00\\x80\\xFF\\\\\\\""
    );
    let long = vec![0xff; 2 * 1024 * 1024];
    let diag = tokens(&long, b"answer").unwrap_err();
    let Diag::TokenMismatch { o, .. } = &*diag else {
        panic!("expected token mismatch")
    };
    assert_eq!(o.excerpt.rendered().len(), 256);
    assert!(o.excerpt.truncated());
    let mut same_prefix = long.clone();
    *same_prefix.last_mut().unwrap() = 0xfe;
    assert!(tokens(&long, &same_prefix).is_err());
    assert!(diag.to_string().contains("…"));
}

#[test]
fn every_diagnostic_has_a_bounded_compact_wire_form() {
    let item = Item {
        offset: u64::MAX,
        line: u64::MAX,
        col: u64::MAX,
        excerpt: Excerpt::new(&[0xff; 64]),
    };
    assert!(serde_json::to_string(&item).unwrap().len() <= 600);
    // Seventeen significant digits, a sign and a negative three-digit
    // exponent maximise the shortest decimal's length. Hex escapes maximise
    // excerpt JSON expansion; false is the longer JSON boolean.
    let value = F::new(-f64::MIN_POSITIVE);
    let variants = [
        Diag::TokenMismatch {
            index: u64::MAX,
            o: item.clone(),
            a: item.clone(),
        },
        Diag::LineMismatch {
            line: u64::MAX,
            o: item.clone(),
            a: item.clone(),
        },
        Diag::ByteMismatch {
            offset: u64::MAX,
            o: None,
            a: None,
        },
        Diag::CountMismatch {
            unit: Unit::Tokens,
            o: u64::MAX,
            a: u64::MAX - 1,
            first_extra: item.clone(),
        },
        Diag::ClassMismatch {
            index: u64::MAX,
            o: item.clone(),
            a: item.clone(),
            expected: Class::Numeric,
        },
        Diag::IntegerMismatch {
            index: u64::MAX,
            o: item.clone(),
            a: item.clone(),
        },
        Diag::Tolerance {
            index: u64::MAX,
            o: item.clone(),
            a: item.clone(),
            o_val: value.clone(),
            a_val: value.clone(),
            scale: value.clone(),
            diff_scaled: value.clone(),
            b_abs_scaled: Some((value.clone(), false)),
            b_rel_scaled: Some((value.clone(), false)),
            diff: Some(value),
        },
        Diag::UlpExcess {
            index: u64::MAX,
            o: item.clone(),
            a: item.clone(),
            dist: u128::MAX,
            ulps: u64::MAX,
        },
        Diag::OutOfRange {
            index: u64::MAX,
            o: item.clone(),
        },
        Diag::InvalidAnswer {
            index: u64::MAX,
            a: item,
        },
        Diag::IncompleteOutput,
    ];
    for diag in variants {
        let wire = serde_json::to_string(&diag).unwrap();
        assert!(wire.len() <= 2048, "{}: {} bytes", kind(&diag), wire.len());
        let decoded: serde_json::Value = serde_json::from_str(&wire).unwrap();
        assert_eq!(serde_json::to_string(&decoded).unwrap().len(), wire.len());
        assert_eq!(decoded["kind"], kind(&diag));
        for field in ["index", "line", "offset", "dist", "ulps"] {
            if let Some(value) = decoded.get(field) {
                assert!(value.is_string(), "{field}: {wire}");
            }
        }
        for field in ["o", "a", "first_extra"] {
            if let Some(item) = decoded.get(field).filter(|value| value.is_object()) {
                assert_eq!(item["offset"], u64::MAX.to_string());
                assert_eq!(item["line"], u64::MAX.to_string());
                assert_eq!(item["col"], u64::MAX.to_string());
                assert!(item["excerpt"].is_string());
                assert_eq!(item["truncated"], false);
            }
        }
        if matches!(diag, Diag::CountMismatch { .. }) {
            assert!(decoded["o"].is_string());
            assert!(decoded["a"].is_string());
        }
        assert!(!diag.to_string().is_empty());
    }
    for number in [
        0.0,
        -0.0,
        f64::MAX,
        -f64::MAX,
        f64::MIN_POSITIVE,
        f64::from_bits(1),
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
    ] {
        let value = F::new(number);
        assert!(value.text().len() <= 32);
        assert_eq!(value.bits().len(), 16);
        assert_eq!(value.value().to_bits(), number.to_bits());
        let wire = serde_json::to_value(&value).unwrap();
        assert!(wire["text"].is_string());
        assert!(wire["bits"].is_string());
        if number.is_finite() {
            assert_eq!(
                value.text().parse::<f64>().unwrap().to_bits(),
                number.to_bits()
            );
        }
    }
    assert_eq!(F::new(f64::NAN).text(), "nan");
    assert_eq!(F::new(f64::INFINITY).text(), "inf");
    assert_eq!(F::new(f64::NEG_INFINITY).text(), "-inf");
    assert_eq!(
        serde_json::to_string(&Diag::ByteMismatch {
            offset: 1,
            o: Some(b'\n'),
            a: None
        })
        .unwrap(),
        r#"{"kind":"byte_mismatch","offset":"1","o":10,"a":null}"#
    );
}

#[test]
fn several_mib_stream_with_constant_state_and_borrowed_items() {
    let input = b"1.0 x\n".repeat(600_000);
    assert!(input.len() >= 3 * 1024 * 1024);
    // Neither the iterator nor its yielded slices own heap allocations. Their
    // sizes are fixed regardless of the input or token count.
    assert!(!std::mem::needs_drop::<Items<'_>>());
    assert!(!std::mem::needs_drop::<SliceItem<'_>>());
    assert!(std::mem::size_of::<Items<'_>>() <= 128);
    assert!(std::mem::size_of::<SliceItem<'_>>() <= 64);
    let first = Items::new(&input, Unit::Tokens).next().unwrap();
    assert!(std::ptr::eq(first.bytes.as_ptr(), input.as_ptr()));
    assert_eq!(Items::new(&input, Unit::Tokens).count(), 1_200_000);
    assert_eq!(exact(&input, &input), Ok(()));
    assert_eq!(tokens(&input, &input), Ok(()));
    assert_eq!(lines(&input, &input), Ok(()));
    assert_eq!(
        check(&f(), &input, Some(&input), true),
        CheckOutcome::Passed
    );
}
