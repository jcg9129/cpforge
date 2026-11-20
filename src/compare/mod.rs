//! Byte-based output comparators implementing `docs/compare-spec.md` v4.
//!
//! [`check`] handles answer presence and capture completeness. The individual
//! comparators return a bounded [`Diag`] on mismatch; [`float`] can also return
//! `InvalidAnswer`, after validating the entire answer before comparing pairs.
//! Float settings must have passed configuration validation. These comparators
//! do not claim compatibility with testlib or any particular judge.
//!
//! Iteration borrows input slices and tracks original byte positions in constant
//! space. No token or line is allocated; only a failing diagnostic allocates.
//!
//! Diagnostics retain the fields listed in §6. Its `Class` type is unspecified;
//! [`Class::Numeric`] expresses the integer-or-decimal expectation when spelling
//! is unrestricted. Section 5.4 calls unscaled bounds "display fields", while
//! §6 lists only scaled bounds; [`Diag::unscaled_bounds`] computes the display
//! values without adding fields to that wire schema. Failure results box their
//! diagnostic to keep the successful result small.

mod diag;
mod number;

use std::fmt;

use crate::config::{Comparator, FloatComparator};

pub use diag::{Class, Diag, Excerpt, F, Item, Unit};

/// A comparison result with a bounded, heap-owned diagnostic on failure.
pub type ComparisonResult = Result<(), Box<Diag>>;

/// The result of checking output, independently of execution metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckOutcome {
    Unchecked,
    Passed,
    Failed(Box<Diag>),
    CheckerFailed(Box<Diag>),
}

impl fmt::Display for CheckOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unchecked => f.write_str("unchecked"),
            Self::Passed => f.write_str("passed"),
            Self::Failed(reason) => write!(f, "failed: {reason}"),
            Self::CheckerFailed(reason) => write!(f, "checker failed: {reason}"),
        }
    }
}

/// Check captured bytes. An empty `Some` answer is present; completeness is
/// supplied by execution and is considered only after a successful comparison.
/// An absent answer returns `Unchecked`; invalid float answers return
/// `CheckerFailed` before output comparison. Other mismatches return `Failed`.
pub fn check(
    comparator: &Comparator,
    output: &[u8],
    answer: Option<&[u8]>,
    output_complete: bool,
) -> CheckOutcome {
    let Some(answer) = answer else {
        return CheckOutcome::Unchecked;
    };
    let result = match comparator {
        Comparator::Exact => exact(output, answer),
        Comparator::Tokens => tokens(output, answer),
        Comparator::Lines => lines(output, answer),
        Comparator::Float(settings) => float(settings, output, answer),
    };
    match result {
        Err(reason) if matches!(*reason, Diag::InvalidAnswer { .. }) => {
            CheckOutcome::CheckerFailed(reason)
        }
        Err(reason) => CheckOutcome::Failed(reason),
        Ok(()) if !output_complete => CheckOutcome::Failed(Box::new(Diag::IncompleteOutput)),
        Ok(()) => CheckOutcome::Passed,
    }
}

/// Compare byte for byte, reporting the first differing byte or prefix EOF.
pub fn exact(output: &[u8], answer: &[u8]) -> ComparisonResult {
    for (offset, (&o, &a)) in output.iter().zip(answer).enumerate() {
        if o != a {
            return Err(Box::new(Diag::ByteMismatch {
                offset: offset as u64,
                o: Some(o),
                a: Some(a),
            }));
        }
    }
    if output.len() != answer.len() {
        let offset = output.len().min(answer.len());
        return Err(Box::new(Diag::ByteMismatch {
            offset: offset as u64,
            o: output.get(offset).copied(),
            a: answer.get(offset).copied(),
        }));
    }
    Ok(())
}

/// Compare maximal non-whitespace runs bytewise, ignoring whitespace kind and
/// amount. Whitespace includes VT and FF, and excludes every non-ASCII byte.
pub fn tokens(output: &[u8], answer: &[u8]) -> ComparisonResult {
    pairs(output, answer, Unit::Tokens, |index, o, a| {
        (o.bytes != a.bytes).then(|| Diag::TokenMismatch {
            index,
            o: o.item(),
            a: a.item(),
        })
    })
}

/// Compare LF-delimited lines, trimming trailing SP/TAB/VT/FF/CR and dropping
/// trailing empty lines. Leading and interior whitespace remains significant.
pub fn lines(output: &[u8], answer: &[u8]) -> ComparisonResult {
    pairs(output, answer, Unit::Lines, |line, o, a| {
        (o.bytes != a.bytes).then(|| Diag::LineMismatch {
            line,
            o: o.item(),
            a: a.item(),
        })
    })
}

/// Compare tokens by the configured numeric rules, with exact normalised
/// integers by default. Decimal answers use binary64 tolerance; disabling
/// `int_exact` also rounds integers to binary64 and may conflate values above
/// 2^53. A whole-answer validation pass takes precedence over any mismatch.
pub fn float(settings: &FloatComparator, output: &[u8], answer: &[u8]) -> ComparisonResult {
    for (index, token) in Items::new(answer, Unit::Tokens).enumerate() {
        if number::invalid_answer(settings, token.bytes) {
            return Err(Box::new(Diag::InvalidAnswer {
                index: index as u64 + 1,
                a: token.item(),
            }));
        }
    }
    pairs(output, answer, Unit::Tokens, |index, o, a| {
        number::compare(settings, index, o, a)
    })
}

fn is_ws(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

#[derive(Clone, Copy, Debug)]
struct Position {
    offset: usize,
    line: u64,
    col: u64,
}

#[derive(Clone, Copy, Debug)]
struct SliceItem<'a> {
    bytes: &'a [u8],
    position: Position,
}

impl SliceItem<'_> {
    fn item(self) -> Item {
        Item {
            offset: self.position.offset as u64,
            line: self.position.line,
            col: self.position.col,
            excerpt: Excerpt::new(self.bytes),
        }
    }
}

/// Constant-sized streaming state; every yielded item borrows its input.
#[derive(Debug)]
struct Items<'a> {
    bytes: &'a [u8],
    position: Position,
    unit: Unit,
}

impl<'a> Items<'a> {
    fn new(bytes: &'a [u8], unit: Unit) -> Self {
        // An all-whitespace suffix is precisely the per-line trailing space
        // followed by trailing empty lines. Finding its boundary needs no
        // position computation; all positions are tracked during forward iteration.
        let bytes = if unit == Unit::Lines {
            let end = bytes.iter().rposition(|&b| !is_ws(b)).map_or(0, |i| i + 1);
            &bytes[..end]
        } else {
            bytes
        };
        Self {
            bytes,
            position: Position {
                offset: 0,
                line: 1,
                col: 1,
            },
            unit,
        }
    }

    fn advance(&mut self, byte: u8) {
        self.position.offset += 1;
        if byte == b'\n' {
            self.position.line += 1;
            self.position.col = 1;
        } else {
            self.position.col += 1;
        }
    }
}

impl<'a> Iterator for Items<'a> {
    type Item = SliceItem<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.unit == Unit::Tokens {
            while let Some(&byte) = self.bytes.get(self.position.offset) {
                if !is_ws(byte) {
                    break;
                }
                self.advance(byte);
            }
        }
        let position = self.position;
        if position.offset == self.bytes.len() {
            return None;
        }
        let mut end = position.offset;
        while let Some(&byte) = self.bytes.get(self.position.offset) {
            if self.unit == Unit::Tokens && is_ws(byte) {
                break;
            }
            self.advance(byte);
            if self.unit == Unit::Lines && byte == b'\n' {
                break;
            }
            if self.unit == Unit::Tokens || !is_ws(byte) {
                end = self.position.offset;
            }
        }
        Some(SliceItem {
            bytes: &self.bytes[position.offset..end],
            position,
        })
    }
}

fn pairs(
    output: &[u8],
    answer: &[u8],
    unit: Unit,
    mut compare: impl FnMut(u64, SliceItem<'_>, SliceItem<'_>) -> Option<Diag>,
) -> ComparisonResult {
    let mut output = Items::new(output, unit);
    let mut answer = Items::new(answer, unit);
    let mut paired = 0;
    loop {
        match (output.next(), answer.next()) {
            (Some(o), Some(a)) => {
                paired += 1;
                if let Some(reason) = compare(paired, o, a) {
                    return Err(Box::new(reason));
                }
            }
            (None, None) => return Ok(()),
            (o, a) => {
                let first_extra = o.or(a).expect("one extra item").item();
                let o = paired + u64::from(o.is_some()) + output.count() as u64;
                let a = paired + u64::from(a.is_some()) + answer.count() as u64;
                return Err(Box::new(Diag::CountMismatch {
                    unit,
                    o,
                    a,
                    first_extra,
                }));
            }
        }
    }
}

#[cfg(test)]
mod tests;
