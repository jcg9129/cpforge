//! Bounded spelling/inode observation, shared by native and stub directory reads.
use std::ffi::OsStr;

#[derive(Debug, Default)]
pub(crate) struct Listing {
    pub(crate) inode: Option<u64>,
    pub(crate) alias: bool,
    pub(crate) cutoff: bool,
}

#[derive(Debug)]
pub(crate) enum Observation {
    Entry {
        inode: u64,
        kind: Option<crate::report::OutputFileType>,
    },
    Retry,
    Failed(crate::report::OutputFile),
}
#[derive(Debug)]
pub(crate) enum Decision {
    Accept,
    Retry,
    Outcome(crate::report::OutputFile),
}
pub(crate) fn decide(listing: Listing, observed: Observation) -> Decision {
    use crate::report::OutputFile;
    let Some(inode) = listing.inode else {
        return Decision::Outcome(if listing.cutoff {
            OutputFile::TooManyEntries
        } else if listing.alias {
            OutputFile::Misnamed
        } else {
            OutputFile::Missing
        });
    };
    match observed {
        Observation::Entry {
            inode: observed, ..
        } if observed != inode => Decision::Retry,
        Observation::Entry {
            kind: Some(file_type),
            ..
        } => Decision::Outcome(OutputFile::NotRegular { file_type }),
        Observation::Entry { .. } => Decision::Accept,
        Observation::Retry => Decision::Retry,
        Observation::Failed(outcome) => Decision::Outcome(outcome),
    }
}

#[derive(Debug)]
pub(crate) struct Scan {
    name: Vec<u8>,
    key: String,
    count: u32,
    result: Listing,
}
impl Scan {
    pub(crate) fn new(name: &OsStr) -> Self {
        Self {
            name: name.as_encoded_bytes().to_vec(),
            key: crate::names::collision_key(name.to_str().expect("validated name")),
            count: 0,
            result: Listing::default(),
        }
    }

    /// Returns true when this read has a decision. Dot entries do not count.
    pub(crate) fn entry(&mut self, name: &[u8], inode: u64) -> bool {
        if name == b"." || name == b".." {
            return false;
        }
        self.count += 1;
        if self.count > 65_536 {
            self.result.cutoff = true;
            return true;
        }
        if name == self.name {
            self.result.inode = Some(inode);
            return true;
        }
        if let Ok(text) = std::str::from_utf8(name) {
            self.result.alias |= crate::names::collision_key(text) == self.key;
        }
        false
    }

    pub(crate) fn finish(self) -> Listing {
        self.result
    }
}

#[cfg(all(test, feature = "storage-test-hooks"))]
mod tests {
    use super::*;
    use crate::report::OutputFile;
    #[test]
    fn spelling_is_final_and_only_matching_inodes_can_be_accepted() {
        for observed in [
            Observation::Retry,
            Observation::Failed(OutputFile::Unreadable { os_error: 5 }),
            Observation::Entry {
                inode: 42,
                kind: None,
            },
        ] {
            assert!(matches!(
                decide(
                    Listing {
                        alias: true,
                        ..Listing::default()
                    },
                    observed
                ),
                Decision::Outcome(OutputFile::Misnamed)
            ));
        }
        assert!(matches!(
            decide(
                Listing {
                    inode: Some(43),
                    ..Listing::default()
                },
                Observation::Entry {
                    inode: 42,
                    kind: None
                }
            ),
            Decision::Retry
        ));
        assert!(matches!(
            decide(
                Listing {
                    inode: Some(42),
                    ..Listing::default()
                },
                Observation::Retry
            ),
            Decision::Retry
        ));
    }
}
