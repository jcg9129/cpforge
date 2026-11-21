use std::fs;
use std::path::{Path, PathBuf};

use cpforge::storage::WorkspaceLock;

#[derive(Debug)]
pub struct Directory(pub PathBuf);

impl Directory {
    pub fn new() -> Self {
        let mut bits = [0_u8; 16];
        getrandom::fill(&mut bits).unwrap();
        let path = std::env::temp_dir().join(format!(
            "cpforge-storage-c4b-{:032x}",
            u128::from_le_bytes(bits)
        ));
        fs::create_dir(&path).unwrap();
        Self(cpforge::storage::paths::plain(
            &fs::canonicalize(path).unwrap(),
        ))
    }

    pub fn path(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.0.join(relative)
    }

    pub fn lock(&self) -> WorkspaceLock {
        WorkspaceLock::new(&self.0).unwrap()
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        // Fixture teardown only; production recovery uses the no-follow walker.
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub fn assert_no_temps(root: &Path) {
    assert!(fs::read_dir(root).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".cpf-tmp-")
    }));
}
