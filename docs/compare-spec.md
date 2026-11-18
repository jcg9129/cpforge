# Comparator specification (CLI foundation)

Status: v4. Normative for `compare` (design §6.3). Float comparison
methods are configurable.

These comparators are cpforge's own. They are **not** drop-in equivalents of
testlib's `wcmp`, `lcmp`, `rcmp*` or of any judge (§8).

## 1. Common rules

### 1.1 Check outcome

| Answer | Comparison | Capture of output complete? | Result |
|---|---|---|---|
| absent | not run | any | `unchecked` |
| present | mismatch | any | `failed(<comparison reason>)` |
| present | match | no | `failed(incomplete_output)` |
| present | match | yes | `passed` |
| present, invalid (§5.5) | not run | any | `checker_failed(invalid_answer)` |

An empty answer is present. "Complete" is decided by the execution contract
(`contracts/execution.md` §5.2), never by the comparator: output that is
exactly as long as the budget is not evidence of truncation. Completeness is
also kept as separate metadata in the record.

### 1.2 Bytes, whitespace, tokens

- Comparators work on bytes; they never decode or lossily convert input. The
  numeric path (§5) borrows an ASCII slice already validated by the grammar
  as `&str` for parsing.
- **Whitespace** is exactly (Rust's `u8::is_ascii_whitespace` excludes
  VT and MUST NOT be used):
  `fn is_ws(b: u8) -> bool { matches!(b, b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r') }`
- Every other byte (NUL, NBSP bytes, ≥ 0x80, invalid UTF-8) is content.
- A **token** is a maximal run of non-whitespace bytes.

### 1.3 Resources

Time linear in the input sizes; memory O(1) beyond the inputs and the bounded
diagnostic: iterate with borrowed slices and indices, never collect tokens or
lines into vectors, never rescan for positions (track offset, line and column
while iterating). Integer comparison uses slice normalisation, no bignum.

### 1.4 Mismatch order

Paired items (tokens or lines) are compared in order; the first differing pair
is reported. A count mismatch is reported only when all paired items match;
scanning continues only to obtain the total counts.

## 2. `exact`

`O == A` byte for byte. On mismatch: first differing byte offset, or the
length difference if one is a prefix of the other.

## 3. `tokens` (default)

Pass iff the token sequences are equal in length and pairwise byte-identical.
Consequences: whitespace kind and amount between, before and after tokens is
irrelevant (CRLF = LF, final newline, blank lines); empty and whitespace-only
are equal; `007` ≠ `7`; case matters.

## 4. `lines`

1. Split each input on LF into lines; a final LF does not start a new line.
2. Remove from each line all trailing bytes in `{SP, TAB, VT, FF, CR}`
   (removes the CR of CRLF).
3. Remove empty lines at the end of the file.
4. Pass iff the line sequences are equal byte for byte.

So the mode ignores **trailing whitespace on each line and trailing empty
lines**. Leading whitespace, interior
whitespace runs, leading and interior blank lines, and a non-trailing CR are
significant. Unlike testlib's `lcmp` it does not tokenise within lines.

## 5. `float`

### 5.1 Configuration

```toml
compare = { float = { abs = 1e-6, rel = 1e-6 } }      # full form
compare = { float = 1e-6 }                            # preset: abs = rel = 1e-6, either
compare = { float_abs = 1e-6 }                        # preset: abs only
compare = { float_rel = 1e-6 }                        # preset: rel only
```

| Key | Type | Default | Rule |
|---|---|---|---|
| `abs` | float | omitted | finite, `>= 0` |
| `rel` | float | omitted | finite, `>= 0` |
| `rel_base` | `"answer"` \| `"max"` \| `"max1"` | `"answer"` | only valid if `rel` is given |
| `combine` | `"either"` \| `"both"` | `"either"` | only valid if `abs` and `rel` are both given |
| `ulps` | integer `0..=i64::MAX` (cpforge's supported TOML integer range) | omitted | excludes `abs`, `rel`, `rel_base`, `combine`; kept as an exact integer, never converted to binary64 |
| `int_exact` | bool | `true` | §5.3 |

Validation, all before anything runs (configuration contract reports location):

1. Unknown keys are errors. A `compare` value holds **exactly one**
   comparator selector (`exact`, `tokens`, `lines`, `float`, `float_abs`,
   `float_rel`); two selectors, e.g. `float_abs` and `float_rel`, are an
   error. Presets take no modifiers; modified settings use the full form.
   Layers (user → workspace → problem) replace
   the whole `compare` value, never merge its fields.
2. At least one of `abs`, `rel`, `ulps`. Omitted differs from `0`.
3. `abs` and `rel` are TOML floats or integers converted to binary64 with
   round to nearest. The converted value must be finite and not negative;
   `-0.0` counts as `0`. Validation works on the converted value, so a literal
   whose magnitude rounds to zero (e.g. `1e-400`, `-1e-400`) **is** `0`; the
   config report shows the effective value. Lexeme-level rejection is not
   used because the TOML layer delivers binary64.
4. Modifiers given explicitly without the bound they modify are errors, also
   when they restate the default.

The report always shows the fully expanded settings.

### 5.2 Token classes

Each grammar must consume the **whole** token; ASCII case folding only.

| Class | Grammar |
|---|---|
| `int` | `[+-]?[0-9]+` |
| `dec` | `[+-]?([0-9]+\.?[0-9]*\|\.[0-9]+)([eE][+-]?[0-9]+)?`, not `int` |
| `special` | `[+-]?(nan\|inf\|infinity)` |
| `other` | everything else (`0x1p3`, `1_000`, `1,5`, `--1`, `1e`, `.`, `e5`, `nan(1)`) |

Exponent digits are unbounded; classification never parses the exponent into
a machine integer. `int` and `dec` are *numeric*. Under `int_exact = true` an
`int` answer never enters binary64 evaluation, so integers of any length are
supported.

### 5.3 Pair rules (output `o`, answer `a`)

First matching row decides.

| # | `a` | `o` | Result |
|---|---|---|---|
| 1 | `other` | any | pass iff byte-identical |
| 2 | `special`; or `dec`, or `int` with `int_exact = false`, that overflows binary64 | — | answer invalid: `checker_failed(invalid_answer)` for the whole check (§5.5) |
| 3 | numeric | `other` or `special` | fail `class_mismatch` |
| 4 | `int`, `int_exact = true` | not `int` | fail `class_mismatch` |
| 5 | `int`, `int_exact = true` | `int` | pass iff equal as unbounded integers (sign, leading zeros normalised, `-0 = 0`); no tolerance; fail `integer_mismatch` |
| 6 | numeric | numeric overflowing binary64 | fail `out_of_range` |
| 7 | numeric | numeric | §5.4 |

**Default (`int_exact = true`):** an integer answer
requires an integer **spelling** in the output and exact equality of
unbounded integers, so `1000001.0` cannot bypass it, and huge integers
are never conflated. Decimal answers use the tolerance. Consequence, stated in
the report's comparator description: a solution printing `5.0` where the
answer file has `5` fails; problems whose answers are reals that happen to be
written as integers need `int_exact = false`.

**`int_exact = false`:** every numeric token, integer or not, is compared by
value with the tolerance, as contestants expect from statements like
"absolute or relative error at most 1e-6". Integers beyond 2^53 are compared
after rounding to binary64 and can be conflated; the report says so. A
per-field schema is a checker's job (initial feature release).

### 5.4 Numeric evaluation

The contract is the stated sequence of binary64 operations (round to nearest,
ties to even, gradual underflow), not exact real arithmetic. Equality means
equality of parsed binary64 values.

1. Parse `o` and `a` with `f64::from_str` (correctly rounded). Underflow to a
   subnormal or to ±0 is accepted as parsed; so `1e-400` equals `2e-400`.
2. **ULP mode** (no subtraction): `key(x) = { let b = x.to_bits as i64; if
   b < 0 { i64::MIN - b } else { b } }`; `dist = (key(o) as i128 - key(a) as
   i128).unsigned_abs`; pass iff `dist <= ulps`, else fail `ulp_excess`
   with `dist`. `-0` and `+0` have key 0.
3. **Bound mode:** scale `s = 0.25` if `max(|o|, |a|) >= 2^1022`, else
   `s = 1` (exact, avoids overflow of the difference and the base).
   - `d = |o·s − a·s|`
   - `base` = `|a|`, `max(|o|, |a|)` or `max(1, |a|)` per `rel_base`;
     `b_abs = abs·s`, `b_rel = rel·(base·s)`; a product overflowing to inf is
     an infinite bound (mathematically larger than any `d`).
   - one bound given: pass iff `d <= that bound`; both: `either` → `d <= b_abs
     || d <= b_rel`, `both` → both hold.
   - fail `tolerance`. The diagnostic stores the scale `s`, the scaled `d`,
     the scaled bounds and each bound's pass/fail; unscaled values (`d/s`,
     bound`/s`) are display fields, `None` when the division overflows.
   - For `max1` the base is `max(1, |a|)` computed before scaling, i.e.
     `max(s, |a|·s)` in the scaled domain.

Boundary cases follow binary64 rounding (e.g. `1.1` vs `1.0` with
`abs = 0.1` fails, since `1.1 − 1.0 > 0.1` in binary64). No hidden epsilon is added; testlib
adds `1e-15`, cpforge does not (§8).

### 5.5 Invalid answers

`float` runs an **answer validation pass** over the whole answer before
comparing (linear time, O(1) space): the first token that row 2 classifies as
invalid makes the check `checker_failed(invalid_answer)`, even when it is
unpaired or comes after an earlier mismatch. Only then are the pairs compared.

An answer token is **invalid** exactly when row 2 of §5.3 says so (`special`;
overflowing `dec`; overflowing `int` only when `int_exact = false`). Such a
reference is unusable for real-number judging: the check is
`checker_failed(invalid_answer)` with the token's position, never a pass.
Users who need such tokens compared textually use `tokens` or a checker.

## 6. Diagnostics

```rust
pub enum Diag {
    TokenMismatch  { index: u64, o: Item, a: Item },          // tokens
    LineMismatch   { line: u64, o: Item, a: Item },            // lines
    ByteMismatch   { offset: u64, o: Option<u8>, a: Option<u8> }, // exact; None = EOF
    CountMismatch  { unit: Unit, o: u64, a: u64, first_extra: Item }, // which side has it
    ClassMismatch  { index: u64, o: Item, a: Item, expected: Class },
    IntegerMismatch{ index: u64, o: Item, a: Item },
    Tolerance      { index: u64, o: Item, a: Item, o_val: F, a_val: F, scale: F,
                     diff_scaled: F, b_abs_scaled: Option<(F, bool)>, b_rel_scaled: Option<(F, bool)>,
                     diff: Option<F> },   // bool = that bound held; diff = d/s, None iff the division overflowed
    UlpExcess      { index: u64, o: Item, a: Item, dist: u128, ulps: u64 },  // compare as dist <= u128::from(ulps)
    OutOfRange     { index: u64, o: Item },
    InvalidAnswer  { index: u64, a: Item },
    IncompleteOutput,
}
pub struct Item { offset: u64, line: u64, col: u64, excerpt: Excerpt }
```

- `index` is 1-based; `offset` 0-based into the **original** input; `line` is
  1-based counting LF only; `col` is the 1-based byte column. EOF positions
  are the input length.
- `F` is `{ text, bits }`: `text` is the shortest round-trip decimal (Rust
  `{:e}` of `f64`, ≤ 32 bytes) or `inf`/`-inf`/`nan`; `bits` the 16-digit
  hex bit pattern. JSON carries both as strings.
- **Excerpt:** at most 64 input bytes, then `truncated: true`. Rendering is
  injective: bytes `0x20..=0x7E` except `\` and `"` as themselves; `\` → `\\`,
  `"` → `\"`; LF, CR, TAB → `\n`, `\r`, `\t`; every other byte `\xHH`
  (uppercase hex). Rendered excerpt ≤ 256 bytes.
- The diagnostic is a structured value; terminal escaping happens at the
  presentation layer. **Wire form**: compact JSON, no whitespace,
  internally tagged `{"kind":"tolerance",...}`, field names as in the Rust
  types in snake_case, `Item` as `{"offset":n,"line":n,"col":n,"excerpt":"…",
  "truncated":b}`, `F` as `{"text":"…","bits":"…"}`, `u128`/`u64` as JSON
  strings of decimal digits. Worst case: an excerpt renders to ≤ 256 bytes and
  JSON escaping (only `\` and `"` occur) at most doubles it, so an `Item` is
  ≤ 600 bytes and the largest variant (`Tolerance`: 2 items, 8 `F`, flags) is
  ≤ 2 KiB. A comparator conformance test serialises the worst case of every variant and asserts
  the limit; nothing is ever truncated into invalid JSON.

## 7. Test table (table-driven)

`f` = default float `{abs = 1e-6, rel = 1e-6}` (so `int_exact = true`); `f, loose` = `f` with `int_exact = false`.

| Mode | O | A | Expected |
|---|---|---|---|
| exact | `1\n` | `1\r\n` | fail (byte 1) |
| tokens | `1 2\r\n` | `1\n2` | pass |
| tokens | `` | ` \n` | pass |
| tokens | `a\x0Bb` | `a b` | pass (VT) |
| tokens | `a\x0Cb` | `a b` | pass (FF) |
| tokens | `a\xC2\xA0b` | `a b` | fail (NBSP content) |
| tokens | `a\x00b` | `ab` | fail (NUL content) |
| tokens | `\xff` | `\xff` | pass |
| tokens | `\xff` | `\xfe` | fail |
| tokens | `007` | `7` | fail |
| tokens | `X extra` | `Y` | fail `TokenMismatch` index 1 (not count) |
| tokens | `Y extra` | `Y` | fail `CountMismatch` 2 vs 1 |
| tokens | `Y` | `Y extra` | fail `CountMismatch` 1 vs 2 |
| lines | `a b  \n\n\n` | `a b` | pass |
| lines | `a\r\n` | `a\n` | pass |
| lines | ` a` | `a` | fail |
| lines | `\na` | `a` | fail (leading blank line) |
| lines | `a\n\nb` | `a\nb` | fail |
| lines | `a  b` | `a b` | fail |
| lines | `a\rb` | `a\nb` | fail |
| f | `5.0` | `5` | fail `ClassMismatch` (strict default) |
| f, loose | `5.0` | `5` | pass |
| f, loose | `1000001` | `1000000` | pass (rel 1e-6 × 1e6 = 1) |
| f, loose | `1000002` | `1000000` | fail |
| f | `1000001` | `1000000` | fail `IntegerMismatch` |
| f | `1000000.0` | `1000000` | fail `ClassMismatch` |
| f | `007` | `7` | pass |
| f | `10000000000000000001` | `10000000000000000000` | fail |
| f, loose | `10000000000000000001` | `10000000000000000000` | pass (binary64, documented) |
| f | `2.5000001` | `2.5` | pass (dec answers use tolerance) |
| abs=0.1 | `1.1` | `1.0` | fail (binary64 boundary) |
| abs=0.125 | `1.125` | `1.0` | pass (exact boundary) |
| abs=0.125 | `1.1250000000000002` | `1.0` | fail |
| rel=0.12 | `1.125` | `1.0` | fail |
| rel=0.12 | `1.0` | `1.125` | pass (asymmetric) |
| rel=0.12, rel_base=max | `1.125` | `1.0` | pass |
| float_rel=1e-6 | `1e-7` | `0.0` | fail (base 0) |
| float = { rel = 1e-6, rel_base = "max1" } | `1e-7` | `0.0` | pass |
| abs=1e-3, rel=1e-6, both | `1000000.0005` | `1000000.0` | pass |
| abs=1e-3, rel=1e-6, both | `1.0005` | `1.0` | fail (rel) |
| abs=1e-3, rel=1e-6, either | `1.0005` | `1.0` | pass (abs) |
| rel=3 | `1e308` | `-1e308` | pass (scaled evaluation) |
| f | `1e308` | `-1e308` | fail |
| f | `-0.0` | `0.0` | pass |
| f | `-0` | `0` | pass (integers, `-0 = 0`) |
| f | `4.9e-324` | `0.0` | pass |
| abs=0 | `1e-400` | `2e-400` | pass (underflow, documented) |
| abs=0 | `0.1` | `0.10` | pass (same binary64) |
| abs=0 | `9007199254740993.0` | `9007199254740992.0` | pass (same binary64) |
| ulps=0 | `-0.0` | `0.0` | pass |
| ulps=1 | `0.30000000000000004` | `0.3` | pass |
| ulps=0 | `0.30000000000000004` | `0.3` | fail |
| ulps=1 | `-5e-324` | `0.0` | pass |
| ulps=1 | `-5e-324` | `5e-324` | fail (dist 2) |
| ulps=2 | `-5e-324` | `5e-324` | pass |
| ulps=1 | `-1.0` | `-0.9999999999999999` | pass (negative adjacency) |
| ulps=1 | `2.2250738585072014e-308` | `2.225073858507201e-308` | pass (normal/subnormal boundary) |
| ulps=i64::MAX | `1e308` | `-1e308` | fail (dist 18429743317745373504 > 2^63−1) |
| ulps=9007199254740993 | `1.0` | `4.000000000000001` | pass (distance exactly 2^53+1; ulps kept exact) |
| ulps=9007199254740992 | `1.0` | `4.000000000000001` | fail |
| f | `1e400` | `1.0` | fail `OutOfRange` |
| f | `1.0` | `1e400` | `checker_failed(invalid_answer)` |
| f | `nan` | `nan` | `checker_failed(invalid_answer)` |
| f | `1` | `inf` | `checker_failed(invalid_answer)` |
| f | `nan` | `1.0` | fail `ClassMismatch` |
| f | `0x10` | `16` | fail `ClassMismatch` |
| f | `1.e2` | `100.0` | pass |
| f | `1e` | `1.0` | fail `ClassMismatch` |
| f | `1e` | `1e` | pass (row 1, `other`) |
| f | `YES` | `YES` | pass |
| f | `yes` | `YES` | fail |
| f | `1` + `0`×400 + `e-400` | `1.0` | pass (long mantissa) |
| f | `1e-` + `0`×500 + `1` | `0.1` | pass (exponent with 501 digits) |
| f | `1` + `0`×400 | `1` + `0`×400 | pass (exact integer beyond binary64) |
| f | `1` + `0`×399 + `1` | `1` + `0`×400 | fail `IntegerMismatch` |
| f, loose | `1` + `0`×400 | `1` + `0`×400 | `checker_failed(invalid_answer)` |
| f | `` | `nan` | `checker_failed(invalid_answer)` (unpaired) |
| f | `X` | `Y nan` | `checker_failed(invalid_answer)` (after mismatch) |
| rel=1.5 | `1.7976931348623157e308` | `-1.7976931348623157e308` | fail; diag `diff = None`, scaled values finite |
| rel=8 | `1.7976931348623157e308` | `-1.7976931348623157e308` | pass (relative product overflows to inf) |
| f | `4.4942328371557893e307` | `4.4942328371557893e307` | pass, `s = 1` (largest value below 2^1022, bits `0x7fcfffffffffffff`) |
| f | `4.49423283715579e307` | `4.4942328371557893e307` | pass, `s = 0.25` (`o` = 2^1022 exactly) |
| rel=1.7976931348623157e308 | `1e308` | `5e-324` | fail (scaled answer base underflows to 0) |
| abs=5e-324 | `1e308` | `0.0` | fail (scaled absolute bound underflows to 0) |
| float = { rel = 1e-6, rel_base = "max1" } | `1e308` | `1e-300` | fail; scaled base `max(0.25, 2.5e-301)` |
| config | `float = { abs = -1e-400 }` | | accepted as `abs = 0` |
| config | `float = { abs = -0.0 }` | | accepted as `abs = 0` |
| config | `float_abs = 1e-6, float_rel = 1e-6` | | error (two selectors) |
| any | matching, capture complete | present | pass |
| any | matching, capture incomplete | present | fail `incomplete_output` |
| any | mismatching, capture incomplete | present | fail (comparison reason) |
| any | anything | absent | `unchecked` |
| tokens | anything | empty (present) | compared (pass iff no tokens) |
| config | `float = { rel_base = "max", abs = 1 }` | | error (modifier without `rel`) |
| config | `float = { abs = -1e-6 }` | | error |
| config | `float = { abs = 1e-400 }` | | accepted as `abs = 0` (shown in report) |
| config | `float = { ulps = 1, abs = 1e-6 }` | | error |
| config | `float = { combine = "either", abs = 1e-6 }` | | error |
| diag | token of 64 bytes / 65 bytes | | not truncated / truncated |
| diag | literal `\xFF` (4 bytes) vs byte `0xFF` | | render `\\xFF` vs `\xFF` |

## 8. Compatibility

| cpforge | Nearest testlib | Differences |
|---|---|---|
| `tokens` | `wcmp` | cpforge whitespace includes VT and FF |
| `lines` | `lcmp` | `lcmp` tokenises each line (ignores leading and repeated interior spaces); cpforge compares trimmed lines bytewise |
| `float` | `rcmp`, `doubleCompare` | `rcmp` checks one value with an absolute threshold; testlib adds `1e-15` and uses its own relative formula; testlib rejects `nan`/`inf` literals in `readDouble` |

No emulation is claimed. A judge's exact behaviour needs its checker (initial feature release).
