//! Candidate-only provenance computed from captured files, before any build.
use std::io::{self, Read};

use super::{CaptureCheck, engine::CaptureReport};
use crate::config::RoleInput;
use crate::report::{
    Header, HeaderReason, JudgeProfile, JudgeText, RoleCapture, RoleOrigin, RoleProvenance,
    RoleReport,
};
use crate::storage::inputs::{CapturedRole, SourceRole};

// Tested candidate only; see docs/testlib-profile.md.
const TESTLIB_SHA256: [u8; 32] = [
    0xd9, 0x2c, 0xa8, 0xa6, 0xb3, 0x69, 0x0d, 0x34, 0xd5, 0xc5, 0x6c, 0x93, 0x94, 0xd9, 0x97, 0x98,
    0x13, 0x0e, 0x21, 0xcb, 0x00, 0x96, 0xdb, 0x81, 0x7d, 0x91, 0xe2, 0x65, 0xf8, 0xd9, 0x58, 0xc3,
];

pub(super) fn snapshot(role: &CapturedRole, check: &CaptureCheck) -> io::Result<RoleReport> {
    let files = role.files.files();
    // The program occupies the source slot in the prebuilt capture (§4.4).
    let mut listed = files.to_vec();
    for file in &mut listed {
        if file.role == SourceRole::Program {
            file.role = SourceRole::Source;
        }
    }
    let capture = CaptureReport::from_sources(check.clone(), &listed);
    let preview = |path: &std::path::PathBuf| {
        let name = path
            .components()
            .map(|c| crate::report::wire::native_bytes(c.as_os_str()))
            .collect::<Vec<_>>()
            .join(&b'/');
        JudgeText::new(&name, 128)
    };
    let (origin, provenance) = match &role.spec.input {
        RoleInput::Prebuilt(p) => (
            RoleOrigin::Prebuilt {
                program: JudgeText::new(p.program.value.as_bytes(), 128),
            },
            RoleProvenance::Declared,
        ),
        RoleInput::Built(b) => (
            RoleOrigin::Built {
                language: b.language.value.clone(),
                entry: b
                    .entry
                    .as_ref()
                    .map(|e| JudgeText::new(e.value.as_bytes(), 128)),
            },
            RoleProvenance::Built {
                header: header(role)?,
            },
        ),
    };
    Ok(RoleReport {
        role: role.role,
        profile: Some(role.spec.profile.value),
        origin,
        build: None,
        capture: RoleCapture {
            checked: matches!(check, CaptureCheck::Checked),
            sources: capture.sources.iter().map(preview).collect(),
            sources_dropped: capture.sources_dropped,
            support_files: capture.support_files.iter().map(preview).collect(),
            support_files_dropped: capture.support_files_dropped,
            counts: capture.counts,
            bytes: capture.bytes,
            lists_sha256: capture.lists_sha256,
            content_sha256: crate::storage::facts::fingerprint::captured_content_fingerprint(
                files,
            )?,
        },
        provenance,
    })
}

fn header(role: &CapturedRole) -> io::Result<Header> {
    if role.spec.profile.value != JudgeProfile::Testlib {
        return Ok(Header::NotApplicable);
    }
    let files = role.files.files();
    let headers: Vec<_> = files
        .iter()
        .filter(|f| {
            f.problem_path
                .components()
                .last()
                .is_some_and(|name| name == "testlib.h")
        })
        .collect();
    let not = |reason| Ok(Header::NotIdentified { reason });
    let candidate = match headers.as_slice() {
        [] => return not(HeaderReason::None),
        [file] => *file,
        _ => return not(HeaderReason::Ambiguous),
    };
    let beside = candidate.copy_path.parent();
    let sources: Vec<_> = files
        .iter()
        .filter(|f| f.role == SourceRole::Source)
        .collect();
    if sources.is_empty() || sources.iter().any(|f| f.copy_path.parent() != beside) {
        return not(HeaderReason::NotBesideSources);
    }
    for source in sources {
        if angle_include(std::fs::File::open(&source.copy_path)?)? {
            return not(HeaderReason::AngleInclude);
        }
    }
    Ok(Header::Candidate {
        path: JudgeText::new(
            candidate.problem_path.components().join("/").as_bytes(),
            128,
        ),
        sha256: candidate.sha256,
        pinned: candidate.sha256 == TESTLIB_SHA256,
    })
}

/// Lexical directive scan with bounded storage, including long source lines.
fn angle_include(mut source: impl Read) -> io::Result<bool> {
    const DIRECTIVE: &[u8] = b"#include<testlib.h>";
    let mut position = Some(0);
    let mut buffer = [0; 8192];
    loop {
        let len = match source.read(&mut buffer) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if len == 0 {
            return Ok(false);
        }
        for &byte in &buffer[..len] {
            if byte == b'\n' {
                position = Some(0);
                continue;
            }
            let Some(index) = position else { continue };
            if matches!(index, 0 | 1 | 8) && byte.is_ascii_whitespace() {
                continue;
            }
            if byte != DIRECTIVE[index] {
                position = None;
            } else if index == DIRECTIVE.len() - 1 {
                return Ok(true);
            } else {
                position = Some(index + 1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};
    use std::fmt::Write;
    use std::path::Path;

    /// The pin, its record in the profile document and the CI copy agree.
    #[test]
    fn pin_matches_profile_document_and_vendored_header() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut hex = String::new();
        for byte in super::TESTLIB_SHA256 {
            write!(hex, "{byte:02x}").unwrap();
        }
        let document =
            std::fs::read_to_string(root.join("docs").join("testlib-profile.md")).unwrap();
        assert!(document.contains(&format!("`{hex}`")), "{hex}");
        let header = std::fs::read(
            root.join("tests")
                .join("fixtures")
                .join("testlib")
                .join("testlib.h"),
        )
        .unwrap();
        assert_eq!(
            <[u8; 32]>::from(Sha256::digest(header)),
            super::TESTLIB_SHA256
        );
    }
}
