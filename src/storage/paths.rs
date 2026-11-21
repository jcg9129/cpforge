//! Native paths suitable for placeholders, user output and child programs.

use std::path::{Path, PathBuf};

/// Use the plain drive or UNC spelling of a Windows verbatim path when that
/// spelling names the same file. A path whose components Win32 parsing would
/// alter (a trailing dot or space, a reserved device name, a character such as
/// `:`) stays verbatim, as do other Windows namespaces and non-Windows paths.
/// Native strings are preserved, including names that are not valid UTF-16.
pub fn plain(path: &Path) -> PathBuf {
    strip_verbatim(path, true)
}

/// The plain spelling used only to compare paths component by component: the
/// drive or UNC verbatim prefix is removed even when [`plain`] keeps it, so a
/// path below a plain root still compares equal component for component.
pub(crate) fn comparable(path: &Path) -> PathBuf {
    strip_verbatim(path, false)
}

fn strip_verbatim(path: &Path, require_same_file: bool) -> PathBuf {
    #[cfg(windows)]
    {
        use std::ffi::OsString;
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        use std::path::{Component, Prefix};

        const SEPARATOR: u16 = 0x5c;
        let wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        // `\\?\C:` without a separator would become drive-relative, and a
        // UNC prefix needs both server and share.
        let (skip, lead, names) = match path.components().next() {
            Some(Component::Prefix(prefix)) => match prefix.kind() {
                Prefix::VerbatimDisk(_) if wide.get(6) == Some(&SEPARATOR) => (4, 0, 7),
                Prefix::VerbatimUNC(server, share) if !server.is_empty() && !share.is_empty() => {
                    (8, 2, 8)
                }
                _ => return path.to_owned(),
            },
            _ => return path.to_owned(),
        };
        if require_same_file
            && !wide[names..]
                .split(|unit| *unit == SEPARATOR)
                .filter(|name| !name.is_empty())
                .all(win32_name)
        {
            return path.to_owned();
        }
        let mut plain = vec![SEPARATOR; lead];
        plain.extend_from_slice(&wide[skip..]);
        PathBuf::from(OsString::from_wide(&plain))
    }
    #[cfg(not(windows))]
    {
        let _ = require_same_file;
        path.to_owned()
    }
}

/// Whether Win32 path parsing keeps this component as spelled.
#[cfg(windows)]
fn win32_name(name: &[u16]) -> bool {
    // " * / : < > ? |
    const REJECTED: [u16; 8] = [0x22, 0x2a, 0x2f, 0x3a, 0x3c, 0x3e, 0x3f, 0x7c];
    !matches!(name.last(), Some(&(0x2e | 0x20)))
        && !name
            .iter()
            .any(|unit| *unit < 0x20 || REJECTED.contains(unit))
        // Lossy decoding only classifies: a replacement character never forms
        // or breaks a device spelling.
        && !crate::names::is_reserved_device_name(&String::from_utf16_lossy(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn windows_drive_and_unc_paths_use_their_plain_form() {
        for (path, expected) in [
            (r"\\?\C:\ws\problems\a", r"C:\ws\problems\a"),
            (r"\\?\D:\", r"D:\"),
            (r"\\?\UNC\srv\share\ws", r"\\srv\share\ws"),
            (r"\\?\UNC\srv\share\", r"\\srv\share\"),
            (r"C:\ws", r"C:\ws"),
            (r"\\srv\share\ws", r"\\srv\share\ws"),
        ] {
            let result = plain(Path::new(path));
            assert_eq!(result, Path::new(expected));
            assert!(result.is_absolute());
            assert_eq!(plain(&result), result);
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_plain_paths_accept_forward_slashes_appended_by_configs() {
        for path in [r"\\?\C:\ws\builds", r"\\?\UNC\srv\share\ws\builds"] {
            let root = plain(Path::new(path));
            let mut expanded = root.as_os_str().to_owned();
            expanded.push("/launcher.exe");
            let expanded = PathBuf::from(expanded);
            assert_eq!(expanded.parent(), Some(root.as_path()));
            assert_eq!(
                expanded.file_name(),
                Some(std::ffi::OsStr::new("launcher.exe"))
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_other_namespaces_are_untouched() {
        for path in [
            r"\\?\Volume{01234567-89ab-cdef-0123-456789abcdef}\ws",
            r"\\?\GLOBALROOT\Device\HarddiskVolume1\ws",
            r"\\.\pipe\cpf",
            r"relative\path",
            r"\\?\C:",
            r"\\?\UNC\srv",
        ] {
            assert_eq!(plain(Path::new(path)), Path::new(path));
            assert_eq!(comparable(Path::new(path)), Path::new(path));
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_components_win32_would_reinterpret_stay_verbatim_except_for_comparison() {
        for (path, expected) in [
            (r"\\?\C:\ws.\problems", r"C:\ws.\problems"),
            (r"\\?\C:\ws \problems", r"C:\ws \problems"),
            (r"\\?\C:\ws\CON", r"C:\ws\CON"),
            (r"\\?\C:\ws\nul.txt\a", r"C:\ws\nul.txt\a"),
            (r"\\?\C:\ws\a:b", r"C:\ws\a:b"),
            (r"\\?\C:\ws\a/b", r"C:\ws\a/b"),
            (r"\\?\UNC\srv\share\com1", r"\\srv\share\com1"),
        ] {
            assert_eq!(plain(Path::new(path)), Path::new(path));
            assert_eq!(comparable(Path::new(path)), Path::new(expected));
        }
        for (path, expected) in [
            (r"\\?\C:\ws\console\a.b", r"C:\ws\console\a.b"),
            (r"\\?\C:\ws\com10", r"C:\ws\com10"),
        ] {
            assert_eq!(plain(Path::new(path)), Path::new(expected));
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_native_strings_are_preserved() {
        use std::ffi::OsString;
        use std::os::windows::ffi::{OsStrExt, OsStringExt};

        for (prefix, expected) in [
            (r"\\?\C:\", r"C:\"),
            (r"\\?\UNC\srv\share\", r"\\srv\share\"),
        ] {
            let path: Vec<u16> = prefix.encode_utf16().chain([0xd800]).collect();
            let expected: Vec<u16> = expected.encode_utf16().chain([0xd800]).collect();
            let result = plain(Path::new(&OsString::from_wide(&path)));
            assert_eq!(
                result.as_os_str().encode_wide().collect::<Vec<_>>(),
                expected
            );
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn non_windows_paths_are_untouched() {
        for path in [
            "/workspace/problems/a",
            "relative/path",
            r"\\?\C:\ws",
            r"\\?\UNC\srv\share\ws",
            r"\\?\Volume{x}\ws",
        ] {
            assert_eq!(plain(Path::new(path)), Path::new(path));
            assert_eq!(comparable(Path::new(path)), Path::new(path));
        }
    }
}
