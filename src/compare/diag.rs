//! Bounded diagnostics and their compact, report-ready wire representation.

use std::fmt;

use serde::{Serialize, Serializer};

/// A binary64 value represented without JSON numbers, including infinities.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct F {
    text: String,
    bits: String,
}

impl F {
    pub(super) fn new(value: f64) -> Self {
        Self {
            text: if value.is_nan() {
                "nan".into()
            } else {
                format!("{value:e}")
            },
            bits: format!("{:016x}", value.to_bits()),
        }
    }

    /// Shortest round-trip decimal, or `inf`, `-inf`, or `nan`.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The sixteen hexadecimal digits of the binary64 representation.
    pub fn bits(&self) -> &str {
        &self.bits
    }

    /// Recover the value, preserving signed zero and the original bits.
    pub fn value(&self) -> f64 {
        f64::from_bits(u64::from_str_radix(&self.bits, 16).expect("stored binary64 bits"))
    }
}

impl fmt::Display for F {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

/// An injectively escaped prefix of at most 64 input bytes (256 rendered bytes).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Excerpt {
    #[serde(rename = "excerpt")]
    rendered: String,
    truncated: bool,
}

impl Excerpt {
    pub(super) fn new(bytes: &[u8]) -> Self {
        let mut rendered = String::with_capacity(bytes.len().min(64) * 4);
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        for &byte in bytes.iter().take(64) {
            match byte {
                b'\\' => rendered.push_str("\\\\"),
                b'"' => rendered.push_str("\\\""),
                b'\n' => rendered.push_str("\\n"),
                b'\r' => rendered.push_str("\\r"),
                b'\t' => rendered.push_str("\\t"),
                0x20..=0x7e => rendered.push(char::from(byte)),
                _ => {
                    rendered.push_str("\\x");
                    rendered.push(char::from(HEX[usize::from(byte >> 4)]));
                    rendered.push(char::from(HEX[usize::from(byte & 15)]));
                }
            }
        }
        Self {
            rendered,
            truncated: bytes.len() > 64,
        }
    }

    /// Escaped ASCII; input bytes are never decoded as text.
    pub fn rendered(&self) -> &str {
        &self.rendered
    }

    /// Whether input bytes beyond the first 64 were omitted.
    pub fn truncated(&self) -> bool {
        self.truncated
    }
}

/// An item's start in the original input, with a bounded excerpt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Item {
    /// Zero-based byte offset.
    #[serde(serialize_with = "decimal")]
    pub offset: u64,
    /// One-based line, counting LF only.
    #[serde(serialize_with = "decimal")]
    pub line: u64,
    /// One-based byte column.
    #[serde(serialize_with = "decimal")]
    pub col: u64,
    #[serde(flatten)]
    pub excerpt: Excerpt,
}

impl fmt::Display for Item {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "\"{}\"{} at byte {}, line {}, column {}",
            self.excerpt.rendered,
            if self.excerpt.truncated { "…" } else { "" },
            self.offset,
            self.line,
            self.col
        )
    }
}

/// The units in a count mismatch. Float comparison also counts tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    Tokens,
    Lines,
}

/// Float token classes; `Numeric` denotes either integer or decimal spelling
/// when used as the expected class in a diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    Int,
    Dec,
    Special,
    Other,
    Numeric,
}

/// A bounded mismatch description following `docs/compare-spec.md` §6.
///
/// Serialize with `serde_json::to_string` for the compact wire form. All u64
/// and u128 fields are decimal strings; byte values remain JSON numbers.
/// Count direction is determined by `o > a` (extra output) or `a > o`
/// (extra answer). Unscaled tolerance bounds are available through
/// [`Diag::unscaled_bounds`] and are included in the human display.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
// Keep the specified, directly inspectable fields; only failures allocate this
// bounded value, and comparison results box it to keep the success path small.
#[allow(clippy::large_enum_variant)]
pub enum Diag {
    TokenMismatch {
        #[serde(serialize_with = "decimal")]
        index: u64,
        o: Item,
        a: Item,
    },
    LineMismatch {
        #[serde(serialize_with = "decimal")]
        line: u64,
        o: Item,
        a: Item,
    },
    ByteMismatch {
        #[serde(serialize_with = "decimal")]
        offset: u64,
        o: Option<u8>,
        a: Option<u8>,
    },
    CountMismatch {
        unit: Unit,
        #[serde(serialize_with = "decimal")]
        o: u64,
        #[serde(serialize_with = "decimal")]
        a: u64,
        first_extra: Item,
    },
    ClassMismatch {
        #[serde(serialize_with = "decimal")]
        index: u64,
        o: Item,
        a: Item,
        expected: Class,
    },
    IntegerMismatch {
        #[serde(serialize_with = "decimal")]
        index: u64,
        o: Item,
        a: Item,
    },
    Tolerance {
        #[serde(serialize_with = "decimal")]
        index: u64,
        o: Item,
        a: Item,
        o_val: F,
        a_val: F,
        scale: F,
        diff_scaled: F,
        b_abs_scaled: Option<(F, bool)>,
        b_rel_scaled: Option<(F, bool)>,
        diff: Option<F>,
    },
    UlpExcess {
        #[serde(serialize_with = "decimal")]
        index: u64,
        o: Item,
        a: Item,
        #[serde(serialize_with = "decimal")]
        dist: u128,
        #[serde(serialize_with = "decimal")]
        ulps: u64,
    },
    OutOfRange {
        #[serde(serialize_with = "decimal")]
        index: u64,
        o: Item,
    },
    InvalidAnswer {
        #[serde(serialize_with = "decimal")]
        index: u64,
        a: Item,
    },
    IncompleteOutput,
}

impl Diag {
    /// Unscaled absolute and relative bounds for display, respectively.
    /// An absent bound or an overflowing division gives `None`.
    /// The scaled fields retain infinite bounds and per-bound pass flags.
    pub fn unscaled_bounds(&self) -> (Option<F>, Option<F>) {
        let Self::Tolerance {
            scale,
            b_abs_scaled,
            b_rel_scaled,
            ..
        } = self
        else {
            return (None, None);
        };
        let unscale = |bound: &Option<(F, bool)>| {
            bound
                .as_ref()
                .and_then(|(bound, _)| finite_display(bound.value() / scale.value()))
        };
        (unscale(b_abs_scaled), unscale(b_rel_scaled))
    }
}

pub(super) fn finite_display(value: f64) -> Option<F> {
    value.is_finite().then(|| F::new(value))
}

fn decimal<T: fmt::Display, S: Serializer>(value: &T, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_str(value)
}

fn byte(f: &mut fmt::Formatter<'_>, value: Option<u8>) -> fmt::Result {
    match value {
        Some(value) => write!(f, "\"{}\"", Excerpt::new(&[value]).rendered),
        None => f.write_str("EOF"),
    }
}

impl fmt::Display for Diag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TokenMismatch { index, o, a } => {
                write!(f, "token {index} differs: output {o}; answer {a}")
            }
            Self::LineMismatch { line, o, a } => {
                write!(f, "line {line} differs: output {o}; answer {a}")
            }
            Self::ByteMismatch { offset, o, a } => {
                write!(f, "byte {offset} differs: output ")?;
                byte(f, *o)?;
                f.write_str("; answer ")?;
                byte(f, *a)
            }
            Self::CountMismatch {
                unit,
                o,
                a,
                first_extra,
            } => write!(
                f,
                "{unit:?} count differs: output {o}; answer {a}; first extra {} {first_extra}",
                if o > a { "output" } else { "answer" }
            ),
            Self::ClassMismatch {
                index,
                o,
                a,
                expected,
            } => write!(
                f,
                "token {index} requires {expected:?}: output {o}; answer {a}"
            ),
            Self::IntegerMismatch { index, o, a } => write!(
                f,
                "integer at token {index} differs: output {o}; answer {a}"
            ),
            Self::Tolerance {
                index,
                o,
                a,
                o_val,
                a_val,
                scale,
                diff_scaled,
                b_abs_scaled,
                b_rel_scaled,
                diff,
            } => {
                write!(
                    f,
                    "tolerance exceeded at token {index}: output {o} ({o_val}); answer {a} ({a_val}); scale {scale}; scaled difference {diff_scaled}; difference "
                )?;
                display_optional(f, diff.as_ref())?;
                let (abs, rel) = self.unscaled_bounds();
                for (name, scaled, unscaled) in [
                    ("absolute", b_abs_scaled, abs),
                    ("relative", b_rel_scaled, rel),
                ] {
                    if let Some((bound, passed)) = scaled {
                        write!(
                            f,
                            "; {name} scaled bound {bound} ({})",
                            if *passed { "held" } else { "failed" }
                        )?;
                        write!(f, "; {name} bound ")?;
                        display_optional(f, unscaled.as_ref())?;
                    }
                }
                Ok(())
            }
            Self::UlpExcess {
                index,
                o,
                a,
                dist,
                ulps,
            } => write!(
                f,
                "ULP distance {dist} exceeds {ulps} at token {index}: output {o}; answer {a}"
            ),
            Self::OutOfRange { index, o } => {
                write!(f, "output token {index} overflows binary64: {o}")
            }
            Self::InvalidAnswer { index, a } => write!(f, "invalid answer token {index}: {a}"),
            Self::IncompleteOutput => f.write_str("output capture is incomplete"),
        }
    }
}

fn display_optional(f: &mut fmt::Formatter<'_>, value: Option<&F>) -> fmt::Result {
    match value {
        Some(value) => write!(f, "{value}"),
        None => f.write_str("overflow"),
    }
}
