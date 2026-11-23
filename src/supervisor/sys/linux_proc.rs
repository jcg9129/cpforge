//! Linux cleanup verification. All procfs access stays behind the sys boundary.
use crate::supervisor::{Cleanup, UnverifiedReason};
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub(in crate::supervisor) struct CleanupScan {
    proc: PathBuf,
    pgid: u32,
    buffer: [u8; 4096],
    last_survivors: Option<u32>,
    entry_delay: Duration,
    #[cfg(test)]
    scans_started: usize,
}

impl CleanupScan {
    pub(in crate::supervisor) fn new(
        proc: &Path,
        fd: Option<i32>,
    ) -> Result<Self, UnverifiedReason> {
        let mountinfo = fs::read_to_string(proc.join("self/mountinfo"))
            .map_err(|_| UnverifiedReason::ProcUnavailable)?;
        let directory = File::open(proc).map_err(|_| UnverifiedReason::ProcUnavailable)?;
        let directory_info =
            fs::read_to_string(proc.join(format!("self/fdinfo/{}", directory.as_raw_fd())))
                .map_err(|_| UnverifiedReason::ProcFiltered)?;
        supported_mount(&mountinfo, &directory_info)?;
        Self::from_supported_mount(proc, fd)
    }

    fn from_supported_mount(proc: &Path, fd: Option<i32>) -> Result<Self, UnverifiedReason> {
        let fd = fd.ok_or(UnverifiedReason::NamespaceUnknown)?;
        let info = fs::read_to_string(proc.join(format!("self/fdinfo/{fd}")))
            .map_err(|_| UnverifiedReason::NamespaceUnknown)?;
        let pgid = info
            .lines()
            .find_map(|line| line.strip_prefix("Pid:"))
            .and_then(|pid| pid.trim().parse::<u32>().ok())
            .filter(|pid| *pid != 0)
            .ok_or(UnverifiedReason::NamespaceUnknown)?;
        Ok(Self {
            proc: proc.into(),
            pgid,
            buffer: [0; 4096],
            last_survivors: None,
            entry_delay: Duration::ZERO,
            #[cfg(test)]
            scans_started: 0,
        })
    }

    pub(in crate::supervisor) fn set_entry_delay(&mut self, delay: Duration) {
        self.entry_delay = delay;
    }

    /// None means a completed scan found live members before the deadline.
    /// No interrupted, malformed, or unsupported scan may return Confirmed.
    /// At the cleanup deadline the last completed scan's survivors stand,
    /// even when a newer scan is interrupted.
    pub(in crate::supervisor) fn poll(
        &mut self,
        cleanup_deadline: Instant,
        reap_deadline: Option<Instant>,
    ) -> Option<Cleanup> {
        let deadline = reap_deadline.map_or(cleanup_deadline, |reap| reap.min(cleanup_deadline));
        let cutoff = if reap_deadline.is_some_and(|reap| reap <= cleanup_deadline) {
            UnverifiedReason::ReapDeadline
        } else {
            UnverifiedReason::ScanIncomplete
        };
        if Instant::now() >= deadline {
            return Some(self.at_deadline(cutoff));
        }
        let result = self.scan(deadline, cutoff);
        if cutoff == UnverifiedReason::ReapDeadline && Instant::now() >= deadline {
            return Some(Cleanup::Unverified { reason: cutoff });
        }
        match result {
            Ok(0) => Some(Cleanup::Confirmed),
            Ok(survivors) if Instant::now() >= deadline => Some(Cleanup::Incomplete { survivors }),
            Ok(survivors) => {
                self.last_survivors = Some(survivors);
                None
            }
            // Only the deadline check yields ScanIncomplete from a scan.
            Err(UnverifiedReason::ScanIncomplete) => Some(self.at_deadline(cutoff)),
            Err(reason) => Some(Cleanup::Unverified { reason }),
        }
    }

    fn at_deadline(&self, cutoff: UnverifiedReason) -> Cleanup {
        match (cutoff, self.last_survivors) {
            (UnverifiedReason::ScanIncomplete, Some(survivors)) => {
                Cleanup::Incomplete { survivors }
            }
            _ => Cleanup::Unverified { reason: cutoff },
        }
    }

    fn scan(
        &mut self,
        deadline: Instant,
        cutoff: UnverifiedReason,
    ) -> Result<u32, UnverifiedReason> {
        #[cfg(test)]
        {
            self.scans_started += 1;
        }
        let entries = fs::read_dir(&self.proc).map_err(|_| UnverifiedReason::Io)?;
        let mut survivors = 0_u32;
        for entry in entries {
            if !self.entry_delay.is_zero() {
                std::thread::sleep(
                    self.entry_delay
                        .min(deadline.saturating_duration_since(Instant::now())),
                );
            }
            if Instant::now() >= deadline {
                return Err(cutoff);
            }
            let entry = entry.map_err(|_| UnverifiedReason::Io)?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.is_empty() || !name.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            let pid = name
                .parse::<u32>()
                .map_err(|_| UnverifiedReason::Malformed)?;
            let record = read_stat(&entry.path().join("stat"), &mut self.buffer);
            if Instant::now() >= deadline {
                return Err(cutoff);
            }
            let record = match record {
                Ok(record) => record,
                Err(error) if matches!(error.raw_os_error(), Some(libc::ENOENT | libc::ESRCH)) => {
                    continue;
                }
                Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                    return Err(UnverifiedReason::Malformed);
                }
                Err(_) => return Err(UnverifiedReason::Io),
            };
            let (state, pgrp) = parse_stat(record, pid)?;
            if pgrp == self.pgid && pid != self.pgid && !matches!(state, b'Z' | b'X') {
                survivors = survivors
                    .checked_add(1)
                    .ok_or(UnverifiedReason::ScanFailed)?;
            }
        }
        // Even the final entry and EOF must have been examined within the bound.
        if Instant::now() >= deadline {
            return Err(cutoff);
        }
        Ok(survivors)
    }
}

fn supported_mount(info: &str, directory_info: &str) -> Result<(), UnverifiedReason> {
    let fields: Vec<_> = info
        .lines()
        .rev()
        .map(|line| line.split_whitespace().collect::<Vec<_>>())
        .find(|fields| fields.get(4) == Some(&"/proc"))
        .ok_or(UnverifiedReason::ProcUnavailable)?;
    let filtered = UnverifiedReason::ProcFiltered;
    let mut ids = directory_info
        .lines()
        .filter_map(|line| line.strip_prefix("mnt_id:"));
    let visible_id = ids
        .next()
        .and_then(|id| id.trim().parse::<u64>().ok())
        .ok_or(filtered)?;
    if ids.next().is_some() || fields[0].parse::<u64>().ok() != Some(visible_id) {
        return Err(filtered);
    }
    let separator = fields
        .iter()
        .position(|field| *field == "-")
        .ok_or(filtered)?;
    if separator < 6 || fields.len() != separator + 4 {
        return Err(filtered);
    }
    if fields[separator + 1] != "proc" {
        return Err(UnverifiedReason::ProcUnavailable);
    }
    for options in [fields[5], fields[separator + 3]] {
        for option in options.split(',') {
            if option.is_empty()
                || option.split_once('=').is_some_and(|(key, value)| {
                    key.is_empty() || value.is_empty() || value.contains('=')
                })
                || (option.starts_with("hidepid") && !matches!(option, "hidepid=0" | "hidepid=off"))
                || (option.starts_with("subset") && option != "subset=pid")
            {
                return Err(filtered);
            }
        }
    }
    Ok(())
}

fn read_stat<'a>(path: &Path, buffer: &'a mut [u8; 4096]) -> io::Result<&'a [u8]> {
    let mut file = File::open(path)?;
    let mut len = 0;
    while len < buffer.len() {
        match file.read(&mut buffer[len..]) {
            Ok(0) => return Ok(&buffer[..len]),
            Ok(n) => len += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    let mut extra = [0];
    loop {
        match file.read(&mut extra) {
            Ok(0) => return Ok(&buffer[..len]),
            Ok(_) => return Err(io::ErrorKind::InvalidData.into()),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
}

fn parse_stat(record: &[u8], pid: u32) -> Result<(u8, u32), UnverifiedReason> {
    let malformed = UnverifiedReason::Malformed;
    let open = record.iter().position(|b| *b == b'(').ok_or(malformed)?;
    let close = record
        .iter()
        .rposition(|b| *b == b')')
        .filter(|close| *close > open)
        .ok_or(malformed)?;
    if std::str::from_utf8(&record[..open])
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        != Some(pid)
    {
        return Err(malformed);
    }
    let tail = std::str::from_utf8(&record[close + 1..]).map_err(|_| malformed)?;
    let mut fields = tail.split_whitespace();
    let state = fields
        .next()
        .filter(|s| s.len() == 1 && b"RSDZTtXxKWPI".contains(&s.as_bytes()[0]))
        .ok_or(malformed)?
        .as_bytes()[0];
    let mut pgrp = None;
    let mut count = 1;
    for field in fields {
        // Numeric stat fields can be signed, and unsigned values may exceed i64.
        field
            .strip_prefix('-')
            .unwrap_or(field)
            .parse::<u64>()
            .map_err(|_| malformed)?;
        if count == 2 {
            pgrp = field.parse::<u32>().ok();
        }
        count += 1;
    }
    if count < 50 {
        return Err(malformed);
    }
    Ok((state, pgrp.ok_or(malformed)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    fn bounded(test: impl FnOnce() + Send + 'static) {
        let (done, result) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            test();
            let _ = done.send(());
        });
        if let Err(std::sync::mpsc::RecvTimeoutError::Timeout) =
            result.recv_timeout(Duration::from_secs(5))
        {
            panic!("synthetic procfs test exceeded 5 seconds");
        }
        worker.join().expect("synthetic procfs test failed");
    }

    struct Tree(PathBuf);
    impl Tree {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::current_dir()
                .unwrap()
                .join("target")
                .join(format!(
                    "proc-test-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
            fs::create_dir_all(path.join("self/fdinfo")).unwrap();
            let tree = Self(path);
            tree.mount("rw,nosuid,nodev,noexec", "rw");
            fs::write(tree.0.join("self/fdinfo/7"), "pos:\t0\nPid:\t100\n").unwrap();
            tree
        }
        fn mount(&self, options: &str, super_options: &str) {
            fs::write(
                self.0.join("self/mountinfo"),
                format!("12 1 0:5 / /proc {options} - proc proc {super_options}\n"),
            )
            .unwrap();
        }
        fn stat(&self, pid: u32, state: char, pgrp: u32) {
            let path = self.0.join(pid.to_string());
            fs::create_dir_all(&path).unwrap();
            // comm contains spaces and closing parentheses, and is not UTF-8.
            let mut record = format!("{pid} (a ) tricky ").into_bytes();
            record.push(0xff);
            record.extend_from_slice(format!(") {state} 1 {pgrp}{}\n", " 0".repeat(47)).as_bytes());
            fs::write(path.join("stat"), record).unwrap();
        }
        fn scan(&self, fd: Option<i32>) -> Result<CleanupScan, UnverifiedReason> {
            let info = fs::read_to_string(self.0.join("self/mountinfo"))
                .map_err(|_| UnverifiedReason::ProcUnavailable)?;
            supported_mount(&info, "mnt_id: 12\n")?;
            CleanupScan::from_supported_mount(&self.0, fd)
        }
        fn scanner(&self) -> CleanupScan {
            self.scan(Some(7)).unwrap()
        }
    }
    impl Drop for Tree {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn completed_scan_uses_fdinfo_numbering_excludes_root_zombies_and_escaped_members() {
        bounded(|| {
            let tree = Tree::new();
            tree.stat(100, 'R', 100); // excluded root, even when live
            tree.stat(101, 'Z', 100);
            tree.stat(102, 'X', 100);
            tree.stat(103, 'S', 103); // escaped with setsid
            fs::create_dir(tree.0.join("104")).unwrap(); // vanished stat
            let mut scan = tree.scanner();
            assert_eq!(
                scan.poll(Instant::now() + Duration::from_secs(1), None),
                Some(Cleanup::Confirmed)
            );
            tree.stat(105, 'S', 100);
            assert_eq!(
                scan.poll(Instant::now() + Duration::from_secs(1), None),
                None
            );
            assert_eq!(
                scan.poll(Instant::now(), None),
                Some(Cleanup::Incomplete { survivors: 1 })
            );
            assert_eq!(
                tree.scanner().poll(Instant::now(), None),
                Some(Cleanup::Unverified {
                    reason: UnverifiedReason::ScanIncomplete
                })
            );
        });
    }

    #[test]
    fn directory_enumeration_failure_never_confirms_cleanup() {
        bounded(|| {
            let tree = Tree::new();
            let mut scan = tree.scanner();
            // Preserve a completed live count: enumeration failure must still
            // be Unverified, rather than reusing that count or confirming.
            tree.stat(101, 'S', 100);
            assert_eq!(
                scan.poll(Instant::now() + Duration::from_secs(1), None),
                None
            );
            let directory = tree.0.join("unavailable-proc");
            fs::write(&directory, b"not a directory").unwrap();
            scan.proc = directory;
            assert_eq!(
                scan.poll(Instant::now() + Duration::from_secs(1), None),
                Some(Cleanup::Unverified {
                    reason: UnverifiedReason::Io
                })
            );
        });
    }

    #[test]
    fn prerequisites_never_confirm_an_unsupported_view() {
        bounded(|| {
            let tree = Tree::new();
            for hidepid in [
                "hidepid=2",
                "hidepid=1",
                "hidepid",
                "hidepid=bad",
                "hidepid=",
            ] {
                for in_super in [false, true] {
                    tree.mount(
                        if in_super { "rw" } else { hidepid },
                        if in_super { hidepid } else { "rw" },
                    );
                    assert_eq!(
                        tree.scan(Some(7)).unwrap_err(),
                        UnverifiedReason::ProcFiltered
                    );
                }
            }
            for options in ["rw", "rw,hidepid=0", "rw,hidepid=off"] {
                tree.mount(options, "rw");
                tree.scanner();
            }
            assert_eq!(
                tree.scan(None).unwrap_err(),
                UnverifiedReason::NamespaceUnknown
            );
            for pid in ["0", "-1", "invalid", ""] {
                fs::write(tree.0.join("self/fdinfo/7"), format!("Pid: {pid}\n")).unwrap();
                assert_eq!(
                    tree.scan(Some(7)).unwrap_err(),
                    UnverifiedReason::NamespaceUnknown
                );
            }
            fs::write(tree.0.join("self/mountinfo"), "12 1 0:5 / /proc rw\n").unwrap();
            assert_eq!(
                tree.scan(Some(7)).unwrap_err(),
                UnverifiedReason::ProcFiltered
            );
            fs::write(tree.0.join("self/mountinfo"), "").unwrap();
            assert_eq!(
                tree.scan(Some(7)).unwrap_err(),
                UnverifiedReason::ProcUnavailable
            );
            fs::remove_file(tree.0.join("self/mountinfo")).unwrap();
            assert_eq!(
                tree.scan(Some(7)).unwrap_err(),
                UnverifiedReason::ProcUnavailable
            );
        });
    }

    #[test]
    fn stacked_mounts_validate_the_visible_mount_id_and_filtering() {
        let mount = |id, options: &str| format!("{id} 1 0:5 / /proc rw - proc proc {options}\n");
        let lower = mount(12, "rw");
        let upper = mount(13, "rw,hidepid=2");
        assert_eq!(
            supported_mount(&(lower.clone() + &upper), "mnt_id: 13\n"),
            Err(UnverifiedReason::ProcFiltered)
        );
        assert_eq!(supported_mount(&(upper + &lower), "mnt_id: 12\n"), Ok(()));
        for info in [
            "",
            "mnt_id: bad\n",
            "mnt_id: 13\n",
            "mnt_id: 12\nmnt_id: 12\n",
        ] {
            assert_eq!(
                supported_mount(&lower, info),
                Err(UnverifiedReason::ProcFiltered)
            );
        }
        for options in ["rw,subset=pid", "rw,hidepid=0,subset=pid", "rw,hidepid=off"] {
            assert_eq!(supported_mount(&mount(12, options), "mnt_id: 12\n"), Ok(()));
        }
        for options in [
            "rw,subset=all",
            "rw,subset=",
            "rw,subset",
            "rw,,hidepid=0",
            "rw,unknown=",
            "rw,hidepid=2",
        ] {
            assert_eq!(
                supported_mount(&mount(12, options), "mnt_id: 12\n"),
                Err(UnverifiedReason::ProcFiltered)
            );
        }
    }

    #[test]
    fn scan_cut_by_cleanup_deadline_uses_only_the_last_completed_live_count() {
        bounded(|| {
            for previous_survivors in [None, Some(0), Some(1)] {
                let tree = Tree::new();
                tree.stat(
                    101,
                    if previous_survivors == Some(0) {
                        'Z'
                    } else {
                        'S'
                    },
                    100,
                );
                let mut scan = tree.scanner();
                if let Some(survivors) = previous_survivors {
                    assert_eq!(
                        scan.poll(Instant::now() + Duration::from_secs(1), None),
                        if survivors == 0 {
                            Some(Cleanup::Confirmed)
                        } else {
                            None
                        }
                    );
                }
                tree.stat(101, 'S', 100);
                let scans_started = scan.scans_started;
                scan.set_entry_delay(Duration::from_millis(100));
                let deadline = Instant::now() + Duration::from_millis(20);
                let expected = if previous_survivors == Some(1) {
                    Cleanup::Incomplete { survivors: 1 }
                } else {
                    Cleanup::Unverified {
                        reason: UnverifiedReason::ScanIncomplete,
                    }
                };
                assert_eq!(scan.poll(deadline, None), Some(expected));
                // Ensure the deadline interrupted scan(), rather than expiring
                // before poll() started and exercising only its entry check.
                assert_eq!(scan.scans_started, scans_started + 1);
                assert!(Instant::now() >= deadline);
            }
        });
    }

    #[test]
    fn reap_deadline_during_rescan_overrides_the_last_completed_live_count() {
        bounded(|| {
            for equal_deadlines in [false, true] {
                let tree = Tree::new();
                tree.stat(101, 'S', 100);
                let mut scan = tree.scanner();
                assert_eq!(
                    scan.poll(Instant::now() + Duration::from_secs(1), None),
                    None
                );
                scan.set_entry_delay(Duration::from_millis(100));
                let start = Instant::now();
                let reap = start + Duration::from_millis(20);
                let cleanup = if equal_deadlines {
                    reap
                } else {
                    start + Duration::from_secs(1)
                };
                assert_eq!(
                    scan.poll(cleanup, Some(reap)),
                    Some(Cleanup::Unverified {
                        reason: UnverifiedReason::ReapDeadline,
                    })
                );
                assert_eq!(scan.scans_started, 2);
            }
        });
    }

    #[test]
    fn slow_scan_distinguishes_the_reap_deadline_from_the_cleanup_deadline() {
        bounded(|| {
            let tree = Tree::new();
            tree.stat(101, 'S', 100);
            for reap_first in [false, true] {
                let mut scan = tree.scanner();
                scan.set_entry_delay(Duration::from_millis(100));
                let start = Instant::now();
                let early = start + Duration::from_millis(20);
                let later = start + Duration::from_secs(1);
                let (cleanup, reap, reason) = if reap_first {
                    (later, early, UnverifiedReason::ReapDeadline)
                } else {
                    (early, later, UnverifiedReason::ScanIncomplete)
                };
                assert_eq!(
                    scan.poll(cleanup, Some(reap)),
                    Some(Cleanup::Unverified { reason })
                );
                assert!(start.elapsed() <= Duration::from_millis(100));
            }
        });
    }

    #[test]
    fn malformed_truncated_and_unreadable_entries_fail_the_scan() {
        bounded(|| {
            let tree = Tree::new();
            tree.stat(101, 'S', 100);
            let path = tree.0.join("101/stat");
            for record in [
                b"".as_slice(),
                b"101 (truncated) S 1 100",
                b"bad stat",
                &[b'x'; 4097],
            ] {
                fs::write(&path, record).unwrap();
                assert_eq!(
                    tree.scanner()
                        .poll(Instant::now() + Duration::from_secs(1), None),
                    Some(Cleanup::Unverified {
                        reason: UnverifiedReason::Malformed
                    })
                );
            }
            // A directory is unreadable as a stat record even when tests run as root.
            fs::remove_file(&path).unwrap();
            fs::create_dir(&path).unwrap();
            assert_eq!(
                tree.scanner()
                    .poll(Instant::now() + Duration::from_secs(1), None),
                Some(Cleanup::Unverified {
                    reason: UnverifiedReason::Io
                })
            );
        });
    }
}
