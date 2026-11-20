use crate::config::{Combine, FloatComparator, RelativeBase};

use super::{Class, Diag, F, SliceItem, diag::finite_display};

/// Whole-token ASCII grammar. Exponents are scanned, never accumulated.
fn classify(bytes: &[u8]) -> Class {
    let start = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    let unsigned = &bytes[start..];
    if [b"nan".as_slice(), b"inf", b"infinity"]
        .iter()
        .any(|special| unsigned.eq_ignore_ascii_case(special))
    {
        return Class::Special;
    }
    let mut i = start;
    while bytes.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
    }
    let integer_digits = i - start;
    if i == bytes.len() && integer_digits > 0 {
        return Class::Int;
    }
    let mut digits = integer_digits;
    if bytes.get(i) == Some(&b'.') {
        i += 1;
        let fraction = i;
        while bytes.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        digits += i - fraction;
    }
    if digits == 0 {
        return Class::Other;
    }
    if matches!(bytes.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(bytes.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        let exponent = i;
        while bytes.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == exponent {
            return Class::Other;
        }
    }
    if i == bytes.len() {
        Class::Dec
    } else {
        Class::Other
    }
}

/// Called only for a slice already validated as the ASCII numeric grammar.
fn value(bytes: &[u8]) -> f64 {
    std::str::from_utf8(bytes)
        .expect("numeric grammar is ASCII")
        .parse::<f64>()
        .expect("numeric grammar is accepted by f64::from_str")
}

pub(super) fn invalid_answer(settings: &FloatComparator, bytes: &[u8]) -> bool {
    match classify(bytes) {
        Class::Special => true,
        Class::Dec => !value(bytes).is_finite(),
        Class::Int if !settings.int_exact => !value(bytes).is_finite(),
        _ => false,
    }
}

/// Normalise signs and leading zeroes with slices, including negative zero.
fn integer(bytes: &[u8]) -> (bool, &[u8]) {
    let negative = bytes.first() == Some(&b'-');
    let start = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    let digits = &bytes[start..];
    let first = digits
        .iter()
        .position(|&b| b != b'0')
        .unwrap_or(digits.len());
    let digits = &digits[first..];
    (negative && !digits.is_empty(), digits)
}

pub(super) fn compare(
    settings: &FloatComparator,
    index: u64,
    o: SliceItem<'_>,
    a: SliceItem<'_>,
) -> Option<Diag> {
    let a_class = classify(a.bytes);
    if a_class == Class::Other {
        return (o.bytes != a.bytes).then(|| Diag::TokenMismatch {
            index,
            o: o.item(),
            a: a.item(),
        });
    }
    // Invalid answers were rejected by the whole-answer validation pass.
    let o_class = classify(o.bytes);
    let strict_integer = a_class == Class::Int && settings.int_exact;
    if matches!(o_class, Class::Other | Class::Special) || (strict_integer && o_class != Class::Int)
    {
        return Some(Diag::ClassMismatch {
            index,
            o: o.item(),
            a: a.item(),
            expected: if strict_integer {
                Class::Int
            } else {
                Class::Numeric
            },
        });
    }
    if strict_integer {
        return (integer(o.bytes) != integer(a.bytes)).then(|| Diag::IntegerMismatch {
            index,
            o: o.item(),
            a: a.item(),
        });
    }
    let o_val = value(o.bytes);
    if !o_val.is_finite() {
        return Some(Diag::OutOfRange { index, o: o.item() });
    }
    let a_val = value(a.bytes);
    if let Some(ulps) = settings.ulps {
        let key = |x: f64| {
            let bits = x.to_bits() as i64;
            if bits < 0 { i64::MIN - bits } else { bits }
        };
        let dist = (i128::from(key(o_val)) - i128::from(key(a_val))).unsigned_abs();
        return (dist > u128::from(ulps)).then(|| Diag::UlpExcess {
            index,
            o: o.item(),
            a: a.item(),
            dist,
            ulps,
        });
    }
    let max = o_val.abs().max(a_val.abs());
    let scale = if max >= f64::from_bits(0x7fd0_0000_0000_0000) {
        0.25
    } else {
        1.0
    };
    let diff_scaled = (o_val * scale - a_val * scale).abs();
    let base = match settings.rel_base {
        RelativeBase::Answer => a_val.abs(),
        RelativeBase::Max => max,
        RelativeBase::Max1 => 1.0_f64.max(a_val.abs()),
    };
    let abs = settings.abs.map(|bound| bound * scale);
    let rel = settings.rel.map(|bound| bound * (base * scale));
    let held = match (abs, rel) {
        (Some(abs), Some(rel)) => match settings.combine {
            Combine::Either => diff_scaled <= abs || diff_scaled <= rel,
            Combine::Both => diff_scaled <= abs && diff_scaled <= rel,
        },
        (Some(bound), None) | (None, Some(bound)) => diff_scaled <= bound,
        (None, None) => unreachable!("validated float settings contain a bound or ulps"),
    };
    (!held).then(|| Diag::Tolerance {
        index,
        o: o.item(),
        a: a.item(),
        o_val: F::new(o_val),
        a_val: F::new(a_val),
        scale: F::new(scale),
        diff_scaled: F::new(diff_scaled),
        b_abs_scaled: abs.map(|bound| (F::new(bound), diff_scaled <= bound)),
        b_rel_scaled: rel.map(|bound| (F::new(bound), diff_scaled <= bound)),
        diff: finite_display(diff_scaled / scale),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grammar_consumes_the_whole_token_and_only_ascii_case_folds_specials() {
        for (bytes, expected) in [
            (b"+007".as_slice(), Class::Int),
            (b"-0", Class::Int),
            (b"1.", Class::Dec),
            (b"+.5", Class::Dec),
            (b"1.e2", Class::Dec),
            (b"2E-03", Class::Dec),
            (b"+NaN", Class::Special),
            (b"-InFiNiTy", Class::Special),
            (b"0x1p3", Class::Other),
            (b"1_000", Class::Other),
            (b"1,5", Class::Other),
            (b"--1", Class::Other),
            (b"1e", Class::Other),
            (b".", Class::Other),
            (b"e5", Class::Other),
            (b"nan(1)", Class::Other),
            (b"", Class::Other),
            (b"+", Class::Other),
            (b"-", Class::Other),
            (b"1\0", Class::Other),
            (b"1\xff", Class::Other),
            (b"\xff", Class::Other),
        ] {
            assert_eq!(classify(bytes), expected, "{bytes:?}");
        }
    }
}
