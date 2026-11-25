//! Finalized file-I/O evidence (report §§2.15, 4.3, 4.6 and 4.10).
use crate::names::PortableComponent;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileIo {
    pub input: Option<PortableComponent>,
    pub output: Option<PortableComponent>,
}
impl FileIo {
    pub const fn stdin_token(&self) -> &'static str {
        if self.input.is_some() {
            "null_device"
        } else {
            "test_input"
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutputFile {
    Collected {
        bytes: u64,
        sha256: [u8; 32],
        path: String,
    },
    Missing,
    Misnamed,
    TooManyEntries,
    NotRegular {
        file_type: OutputFileType,
    },
    Unreadable {
        os_error: i32,
    },
    TooLarge {
        bytes: u64,
        budget: u64,
    },
    NotCollected {
        reason: CollectionFailure,
        os_error: Option<i32>,
    },
    NotAttempted,
}
impl OutputFile {
    pub const fn rejection_reason(&self) -> Option<OutputRejectReason> {
        match self {
            Self::Missing => Some(OutputRejectReason::Missing),
            Self::Misnamed => Some(OutputRejectReason::Misnamed),
            Self::NotRegular { .. } => Some(OutputRejectReason::NotRegular),
            Self::Unreadable { .. } => Some(OutputRejectReason::Unreadable),
            Self::TooLarge { .. } => Some(OutputRejectReason::TooLarge),
            Self::TooManyEntries => Some(OutputRejectReason::DirTooLarge),
            _ => None,
        }
    }
    pub const fn wire_token(&self) -> &'static str {
        match self {
            Self::Collected { .. } => "collected",
            Self::Missing => "missing",
            Self::Misnamed => "misnamed",
            Self::TooManyEntries => "too_many_entries",
            Self::NotRegular { .. } => "not_regular",
            Self::Unreadable { .. } => "unreadable",
            Self::TooLarge { .. } => "too_large",
            Self::NotCollected { .. } => "not_collected",
            Self::NotAttempted => "not_attempted",
        }
    }
}

macro_rules! tokens {
    ($name:ident { $($variant:ident => $token:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
        pub enum $name { $(#[serde(rename = $token)] $variant),+ }
        impl $name {
            pub const fn wire_token(self) -> &'static str {
                match self { $(Self::$variant => $token),+ }
            }
        }
    };
}
tokens!(OutputFileType { Link => "link", Directory => "directory", Other => "other" });
tokens!(CollectionFailure { Io => "io", Timeout => "timeout", Unstable => "unstable" });
tokens!(OutputRejectReason {
    Missing => "output_missing", Misnamed => "output_misnamed",
    NotRegular => "output_not_regular", Unreadable => "output_unreadable",
    TooLarge => "output_too_large", DirTooLarge => "output_dir_too_large",
});
tokens!(IoScratchDetail {
    StagedInputKept => "staged_input_kept", OutputKept => "output_kept",
    PartialCopyKept => "partial_copy_kept", NotMeasured => "not_measured",
});
