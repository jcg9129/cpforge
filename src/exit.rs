//! Process exit statuses. The failure status is provisional until report
//! transport defines the full table. C9c will add a distinct partial-failure
//! status; for now a published problem whose contest membership fails exits 1.

/// Exit statuses used by `cpf`.
///
/// | Variant | Code | Meaning |
/// | --- | --- | --- |
/// | `Success` | 0 | Successful completion, including help and version |
/// | `Failure` | 1 | The command failed or refused, possibly after partial progress (provisional) |
/// | `Usage` | 2 | Invalid arguments or a missing command |
/// | `Internal` | 70 | Bug or unexpected error |
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
#[repr(u8)]
pub enum ExitCode {
    Success = 0,
    /// Provisional until the report-transport task fixes the full exit table.
    Failure = 1,
    Usage = 2,
    Internal = 70,
}

impl From<ExitCode> for std::process::ExitCode {
    fn from(code: ExitCode) -> Self {
        Self::from(code as u8)
    }
}

#[cfg(test)]
mod tests {
    use super::ExitCode;

    #[test]
    fn exit_code_values_are_stable() {
        assert_eq!(ExitCode::Success as u8, 0);
        assert_eq!(ExitCode::Failure as u8, 1);
        assert_eq!(ExitCode::Usage as u8, 2);
        assert_eq!(ExitCode::Internal as u8, 70);
    }
}
