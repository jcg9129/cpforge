use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Pure policy. Canonicalisation is supplied separately so failures are
/// untrusted, never an execution or configuration error.
pub(crate) fn decision(
    overridden: bool,
    enabled: bool,
    depth: Option<&OsStr>,
    workspace: Option<&Path>,
    trusted: &[PathBuf],
) -> Result<(), &'static str> {
    if overridden {
        return Err("config_override");
    }
    if !enabled {
        return Err("disabled");
    }
    if depth.is_some_and(|value| {
        value
            .to_str()
            .is_none_or(|text| text.is_empty() || !text.bytes().all(|byte| byte == b'0'))
    }) {
        return Err("depth");
    }
    if workspace.is_some_and(|root| trusted.iter().any(|path| path == root)) {
        Ok(())
    } else {
        Err("untrusted")
    }
}

#[cfg(test)]
mod tests {
    use super::decision;
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    #[test]
    fn trust_is_exact_and_canonicalisation_failure_is_untrusted() {
        let root = Path::new("/workspace");
        let trusted = [PathBuf::from(root)];
        assert_eq!(decision(false, true, None, Some(root), &trusted), Ok(()));
        assert_eq!(
            decision(false, true, None, Some(root), &[]),
            Err("untrusted")
        );
        assert_eq!(
            decision(false, true, None, None, &trusted),
            Err("untrusted")
        );
        for other in ["/WORKSPACE", "/workspace-extra", "/workspace/child"] {
            assert_eq!(
                decision(false, true, None, Some(Path::new(other)), &trusted),
                Err("untrusted")
            );
        }
    }

    #[test]
    fn override_and_master_switch_take_precedence() {
        assert_eq!(
            decision(true, false, Some(OsStr::new("bad")), None, &[]),
            Err("config_override")
        );
        assert_eq!(
            decision(false, false, Some(OsStr::new("bad")), None, &[]),
            Err("disabled")
        );
    }

    #[test]
    fn depth_accepts_only_absence_or_valid_zero() {
        let root = Path::new("/workspace");
        let trusted = [PathBuf::from(root)];
        for depth in [None, Some(OsStr::new("0")), Some(OsStr::new("00"))] {
            assert_eq!(decision(false, true, depth, Some(root), &trusted), Ok(()));
        }
        for depth in [
            "1",
            "2",
            "bad",
            "",
            "-1",
            "+0",
            " 0",
            "999999999999999999999999",
        ] {
            assert_eq!(
                decision(false, true, Some(OsStr::new(depth)), Some(root), &trusted),
                Err("depth")
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_unicode_depth_is_malformed() {
        use std::os::unix::ffi::OsStrExt;
        assert_eq!(
            decision(false, true, Some(OsStr::from_bytes(&[255])), None, &[]),
            Err("depth")
        );
    }
}
