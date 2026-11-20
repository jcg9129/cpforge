//! One environment-name equivalence for validation, composition and preparation.

use std::ffi::{OsStr, OsString};

use icu_casemap::CaseMapper;

use super::Platform;

/// Fold names on Windows by the Unicode simple uppercase mapping of individual
/// BMP code units. Simple (not full) mappings matter: `ᾀ` (U+1F80) has a
/// one-character simple uppercase `ᾈ` (U+1F88) although its full uppercase
/// expands. Surrogate units, and mappings outside the BMP, stay unchanged.
/// Unix names are byte-exact.
///
/// This must equate at least every pair equated by Windows ordinal ignore-case
/// comparison: otherwise Rust's `Command` could collapse names we kept apart.
/// Equating a few additional pairs is harmless. The Windows conformance test
/// checks that direction against the OS's complete BMP equivalence classes.
/// This is deliberately separate from the ASCII-only reserved `CPF_` check.
pub fn env_name_fold(platform: Platform, name: &str) -> String {
    if platform != Platform::Windows {
        return name.to_owned();
    }
    let units: Vec<u16> = name.encode_utf16().map(fold_unit).collect();
    // Surrogate pairs are unchanged and BMP mappings cannot create surrogates.
    String::from_utf16(&units).expect("fold preserves valid UTF-16")
}

fn fold_unit(unit: u16) -> u16 {
    let Some(character) = char::from_u32(u32::from(unit)) else {
        return unit;
    };
    let mapped = CaseMapper::new().simple_uppercase(character);
    u16::try_from(u32::from(mapped)).unwrap_or(unit)
}

/// Native counterpart for baseline names, retaining unpaired Windows surrogates
/// and non-UTF-8 Unix bytes. Injected Windows tests on Unix fold valid UTF-8
/// segments and preserve invalid bytes verbatim.
pub fn env_name_fold_os(platform: Platform, name: &OsStr) -> OsString {
    if platform != Platform::Windows {
        return name.to_owned();
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let units: Vec<u16> = name.encode_wide().map(fold_unit).collect();
        OsString::from_wide(&units)
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let mut bytes: Vec<u8> = Vec::new();
        for chunk in name.as_bytes().utf8_chunks() {
            bytes.extend_from_slice(env_name_fold(platform, chunk.valid()).as_bytes());
            bytes.extend_from_slice(chunk.invalid());
        }
        OsString::from_vec(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_bmp_fold_and_unix_identity() {
        assert_eq!(env_name_fold(Platform::Windows, "pÄth"), "PÄTH");
        assert_eq!(env_name_fold(Platform::Windows, "ßﬀ𐐨"), "ßﬀ𐐨");
        // Simple, not full, uppercase: both spellings share one fold.
        assert_eq!(env_name_fold(Platform::Windows, "\u{1f80}"), "\u{1f88}");
        assert_eq!(env_name_fold(Platform::Windows, "\u{1f88}"), "\u{1f88}");
        assert_eq!(env_name_fold(Platform::Linux, "pÄth"), "pÄth");
        assert_eq!(fold_unit(0xd800), 0xd800);
    }

    #[cfg(windows)]
    #[test]
    fn every_windows_ordinal_bmp_class_has_one_fold() {
        use std::cmp::Ordering;
        use windows_sys::Win32::Globalization::CompareStringOrdinal;

        let compare = |left: &u16, right: &u16| -> Ordering {
            // SAFETY: each pointer references one live UTF-16 unit; explicit
            // lengths allow even NUL and unpaired surrogates, with no writes.
            match unsafe { CompareStringOrdinal(left, 1, right, 1, 1) } {
                1 => Ordering::Less,
                2 => Ordering::Equal,
                3 => Ordering::Greater,
                code => panic!("CompareStringOrdinal failed: {code}"),
            }
        };
        let mut units: Vec<u16> = (0..=u16::MAX).collect();
        units.sort_by(compare);
        let mut mismatches: Vec<String> = Vec::new();
        for pair in units.windows(2) {
            if compare(&pair[0], &pair[1]) == Ordering::Equal
                && fold_unit(pair[0]) != fold_unit(pair[1])
            {
                mismatches.push(format!("U+{:04X}/U+{:04X}", pair[0], pair[1]));
            }
        }
        assert!(
            mismatches.is_empty(),
            "{} OS-equal pairs have different folds (first 20): {:?}",
            mismatches.len(),
            &mismatches[..mismatches.len().min(20)]
        );
    }

    #[cfg(windows)]
    #[test]
    fn native_fold_keeps_surrogates() {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let name = OsString::from_wide(&[0xd800, 0x00e4, 0xdc00]);
        let units: Vec<u16> = env_name_fold_os(Platform::Windows, &name)
            .encode_wide()
            .collect();
        assert_eq!(units, [0xd800, 0x00c4, 0xdc00]);
    }
}
