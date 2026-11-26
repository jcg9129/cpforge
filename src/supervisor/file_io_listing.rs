//! Bounded spelling/inode observation, shared by native and stub directory reads.
use std::ffi::OsStr;

#[derive(Debug, Default)]
pub(crate) struct Listing {
    pub(crate) inode: Option<u64>,
    pub(crate) alias: bool,
    pub(crate) cutoff: bool,
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
