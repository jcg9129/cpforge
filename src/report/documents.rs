//! Schema-1 document identities. Payload stubs reserve names without claiming
//! to encode or validate documents whose feature implementation is pending.

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentKind {
    JobReport,
    CaptureManifest,
    EventStream,
    Error,
    ConfigShow,
    Status,
    Ls,
    Search,
    Notes,
    StressFailures,
    Submit,
    ArchivePlan,
    Archive,
    Restore,
    Fetch,
}
impl DocumentKind {
    pub const fn token(self) -> &'static str {
        match self {
            Self::JobReport => "job_report",
            Self::CaptureManifest => "capture_manifest",
            Self::EventStream => "event_stream",
            Self::Error => "error",
            Self::ConfigShow => "config_show",
            Self::Status => "status",
            Self::Ls => "ls",
            Self::Search => "search",
            Self::Notes => "notes",
            Self::StressFailures => "stress_failures",
            Self::Submit => "submit",
            Self::ArchivePlan => "archive_plan",
            Self::Archive => "archive",
            Self::Restore => "restore",
            Self::Fetch => "fetch",
        }
    }
}

macro_rules! pending_document {
    ($($name:ident => $kind:ident),+ $(,)?) => {$(
        /// Payload definition pending; this marker cannot be serialized as a document.
        #[derive(Debug)]
        pub struct $name { _pending: () }
        impl $name {
            pub const KIND: DocumentKind = DocumentKind::$kind;
        }
    )+};
}
pending_document! {
    LsDocument => Ls,
    SearchDocument => Search,
    NotesDocument => Notes,
    StressFailuresDocument => StressFailures,
    SubmitDocument => Submit,
    ArchivePlanDocument => ArchivePlan,
    ArchiveDocument => Archive,
    RestoreDocument => Restore,
    FetchDocument => Fetch,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_kind_inventory_round_trips() {
        for kind in [
            DocumentKind::JobReport,
            DocumentKind::CaptureManifest,
            DocumentKind::EventStream,
            DocumentKind::Error,
            DocumentKind::ConfigShow,
            DocumentKind::Status,
            DocumentKind::Ls,
            DocumentKind::Search,
            DocumentKind::Notes,
            DocumentKind::StressFailures,
            DocumentKind::Submit,
            DocumentKind::ArchivePlan,
            DocumentKind::Archive,
            DocumentKind::Restore,
            DocumentKind::Fetch,
        ] {
            let bytes = serde_json::to_vec(&kind).unwrap();
            assert_eq!(bytes, format!("\"{}\"", kind.token()).as_bytes());
            assert_eq!(
                serde_json::from_slice::<DocumentKind>(&bytes).unwrap(),
                kind
            );
        }
        assert!(serde_json::from_str::<DocumentKind>("\"future\"").is_err());
    }
}
