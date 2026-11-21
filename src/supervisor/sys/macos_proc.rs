//! Darwin group verification. Enumeration and inspection are separate observations.
use super::Observer;
use crate::supervisor::{Cleanup, UnverifiedReason};
use std::mem::{MaybeUninit, size_of, size_of_val};
use std::time::{Duration, Instant};

// <sys/proc_info.h> in the macOS SDK: libc 0.2.191 exports the functions,
// PROC_PIDTBSDINFO, proc_bsdinfo and SZOMB, but omits PROC_PGRP_ONLY.
const PROC_PGRP_ONLY: u32 = 2;
const PID_CAP: usize = 64 * 1024;
const INITIAL_PIDS: usize = 512;
const POLL_ENTRIES: usize = 64;
const POLL_SLICE: Duration = Duration::from_millis(1);

#[derive(Debug)]
pub(in crate::supervisor) struct CleanupScan {
    pgid: u32,
    // Allocate the cap before transfer; growing the supplied window never allocates.
    buffer: Vec<libc::pid_t>,
    window: usize,
    entries: Option<usize>,
    index: usize,
    survivors: u32,
    last_survivors: Option<u32>,
    next_scan: Instant,
    backoff_ms: u64,
    entry_delay: Duration,
    entry_ready: Option<Instant>,
    #[cfg(test)]
    list_reply: Option<(i32, i32)>,
    #[cfg(test)]
    full_lists: bool,
    #[cfg(test)]
    info_reply: Option<(i32, i32)>,
    #[cfg(test)]
    info_delay: Duration,
}

pub(in crate::supervisor) fn cleanup_scan(
    pid: i32,
    _observer: Option<&Observer>,
) -> Result<CleanupScan, UnverifiedReason> {
    let pgid = u32::try_from(pid)
        .ok()
        .filter(|pgid| *pgid != 0)
        .ok_or(UnverifiedReason::Unsupported)?;
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(PID_CAP)
        .map_err(|_| UnverifiedReason::Io)?;
    buffer.resize(PID_CAP, 0);
    Ok(CleanupScan {
        pgid,
        buffer,
        window: INITIAL_PIDS,
        entries: None,
        index: 0,
        survivors: 0,
        last_survivors: None,
        next_scan: Instant::now(),
        backoff_ms: 1,
        entry_delay: Duration::ZERO,
        entry_ready: None,
        #[cfg(test)]
        list_reply: None,
        #[cfg(test)]
        full_lists: false,
        #[cfg(test)]
        info_reply: None,
        #[cfg(test)]
        info_delay: Duration::ZERO,
    })
}

fn check_deadline(deadline: Instant, cutoff: UnverifiedReason) -> Result<(), UnverifiedReason> {
    if Instant::now() >= deadline {
        Err(cutoff)
    } else {
        Ok(())
    }
}

fn api_error(errno: i32, otherwise: UnverifiedReason) -> UnverifiedReason {
    match errno {
        libc::ENOSYS | libc::ENOTSUP => UnverifiedReason::Unsupported,
        _ => otherwise,
    }
}

fn list_pids(pgid: u32, buffer: &mut [libc::pid_t]) -> (i32, i32) {
    let bytes = size_of_val(buffer);
    // The bounded buffer is at most 256 KiB; retain checked conversion at the FFI.
    let Ok(bytes) = i32::try_from(bytes) else {
        return (-1, libc::EOVERFLOW);
    };
    // SAFETY: errno is thread-local, buffer is writable for bytes, and libproc
    // does not retain it. Capture errno immediately, including on a zero return.
    unsafe {
        *libc::__error() = 0;
        let count = libc::proc_listpids(PROC_PGRP_ONLY, pgid, buffer.as_mut_ptr().cast(), bytes);
        (count, *libc::__error())
    }
}

fn pid_info(pid: libc::pid_t) -> Result<Option<libc::proc_bsdinfo>, UnverifiedReason> {
    let mut info = MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = i32::try_from(size_of::<libc::proc_bsdinfo>())
        .map_err(|_| UnverifiedReason::Unsupported)?;
    // SAFETY: output is correctly aligned and sized. arg=1 asks XNU to include
    // zombies for PROC_PIDTBSDINFO (bsd/kern/proc_info.c, findzomb). libproc never
    // retains the pointer. No fields are accessed before validating both outputs.
    let (count, errno) = unsafe {
        *libc::__error() = 0;
        let count = libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            1,
            info.as_mut_ptr().cast(),
            size,
        );
        (count, *libc::__error())
    };
    validate_info(count, errno)?;
    if errno == libc::ESRCH {
        return Ok(None);
    }
    // SAFETY: errno is zero and libproc returned exactly the full struct size.
    Ok(Some(unsafe { info.assume_init() }))
}

fn validate_info(count: i32, errno: i32) -> Result<(), UnverifiedReason> {
    if errno == libc::ESRCH {
        return Ok(());
    }
    if errno != 0 || usize::try_from(count).ok() != Some(size_of::<libc::proc_bsdinfo>()) {
        return Err(api_error(errno, UnverifiedReason::InfoFailed));
    }
    Ok(())
}

/// Non-spawning capability probe: validate the caller's group view and BSD info.
/// A real scan still validates every result independently.
pub(in crate::supervisor) fn probe_cleanup() -> Result<(), UnverifiedReason> {
    // SAFETY: getpgrp and getpid take no pointers and do not change process state.
    let (pgid, pid) = unsafe { (libc::getpgrp(), libc::getpid()) };
    let mut buffer = [0; INITIAL_PIDS];
    let (count, errno) = list_pids(pgid as u32, &mut buffer);
    let count = validate_list(count, errno, buffer.len())?;
    // A full buffer may be truncated; otherwise the caller must be enumerated.
    if count < buffer.len() && !buffer[..count].contains(&pid) {
        return Err(UnverifiedReason::Unsupported);
    }
    let info = pid_info(pid)?.ok_or(UnverifiedReason::InfoFailed)?;
    if info.pbi_pid != pid as u32 || info.pbi_pgid != pgid as u32 {
        return Err(UnverifiedReason::Unsupported);
    }
    Ok(())
}

fn validate_list(count: i32, errno: i32, capacity: usize) -> Result<usize, UnverifiedReason> {
    if errno != 0 {
        return Err(api_error(errno, UnverifiedReason::Io));
    }
    let bytes = usize::try_from(count).map_err(|_| UnverifiedReason::Malformed)?;
    let capacity_bytes = capacity
        .checked_mul(size_of::<libc::pid_t>())
        .ok_or(UnverifiedReason::Malformed)?;
    if bytes > capacity_bytes || bytes % size_of::<libc::pid_t>() != 0 {
        return Err(UnverifiedReason::Malformed);
    }
    Ok(bytes / size_of::<libc::pid_t>())
}

impl CleanupScan {
    pub(in crate::supervisor) fn set_entry_delay(&mut self, delay: Duration) {
        self.entry_delay = delay;
    }

    /// None yields to the caller: backoff, delayed entry, or bounded work remains.
    /// A partial scan never confirms or supplies a survivor count. At the cleanup
    /// deadline the last completed scan's survivors stand, even while a newer scan
    /// is unfinished, so live members observed before the deadline still block.
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
        if Instant::now() < self.next_scan {
            return None;
        }
        let result = self.step(deadline, cutoff);
        // Reap expiry always transfers with the root pinned, even if the last
        // entry completed just before poll regained control.
        if cutoff == UnverifiedReason::ReapDeadline && Instant::now() >= deadline {
            return Some(Cleanup::Unverified { reason: cutoff });
        }
        match result {
            // Only the deadline check yields ScanIncomplete from a step.
            Err(UnverifiedReason::ScanIncomplete) => Some(self.at_deadline(cutoff)),
            Err(reason) => Some(Cleanup::Unverified { reason }),
            Ok(false) => None,
            Ok(true) => {
                self.entries = None;
                self.index = 0;
                let survivors = std::mem::take(&mut self.survivors);
                if survivors == 0 {
                    return Some(Cleanup::Confirmed);
                }
                self.last_survivors = Some(survivors);
                self.next_scan = Instant::now() + Duration::from_millis(self.backoff_ms);
                self.backoff_ms = (self.backoff_ms * 2).min(10);
                None
            }
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

    fn step(
        &mut self,
        deadline: Instant,
        cutoff: UnverifiedReason,
    ) -> Result<bool, UnverifiedReason> {
        check_deadline(deadline, cutoff)?;
        let slice_end = Instant::now() + POLL_SLICE;
        if self.entries.is_none() {
            let (count, errno) = self.list();
            check_deadline(deadline, cutoff)?;
            let entries = validate_list(count, errno, self.window)?;
            if entries == self.window {
                self.window = self
                    .window
                    .checked_mul(2)
                    .filter(|size| *size <= PID_CAP)
                    .ok_or(UnverifiedReason::Truncated)?;
                return Ok(false);
            }
            self.entries = Some(entries);
        }
        let entries = self.entries.expect("validated enumeration");
        for _ in 0..POLL_ENTRIES {
            check_deadline(deadline, cutoff)?;
            if self.index == entries {
                return Ok(true);
            }
            if Instant::now() >= slice_end {
                return Ok(false);
            }
            let ready = *self.entry_ready.get_or_insert_with(|| {
                Instant::now()
                    .checked_add(self.entry_delay)
                    .unwrap_or(deadline)
            });
            if Instant::now() < ready {
                return Ok(false);
            }
            self.entry_ready = None;
            let pid = self.buffer[self.index];
            // libproc may supply zero padding; negative identifiers are impossible.
            if pid == 0 || pid as u32 == self.pgid {
                self.index += 1;
                continue;
            }
            if pid < 0 {
                return Err(UnverifiedReason::Malformed);
            }
            let result = self.info(pid);
            // On reap expiry the service must retry this entry, not skip a
            // result that was obtained but never accepted before the cutoff.
            check_deadline(deadline, cutoff)?;
            if let Some(info) = result? {
                // Copy fields before use, including on SDKs with packed structures.
                let (pgid, status) = (info.pbi_pgid, info.pbi_status);
                if pgid == self.pgid && status != libc::SZOMB {
                    self.survivors = self
                        .survivors
                        .checked_add(1)
                        .ok_or(UnverifiedReason::InfoFailed)?;
                }
            }
            self.index += 1;
        }
        check_deadline(deadline, cutoff)?;
        Ok(false)
    }

    fn list(&mut self) -> (i32, i32) {
        #[cfg(test)]
        {
            if self.full_lists {
                return ((self.window * size_of::<libc::pid_t>()) as i32, 0);
            }
            if let Some(reply) = self.list_reply {
                return reply;
            }
        }
        list_pids(self.pgid, &mut self.buffer[..self.window])
    }

    fn info(&self, pid: libc::pid_t) -> Result<Option<libc::proc_bsdinfo>, UnverifiedReason> {
        // Simulate a syscall that cannot be preempted by the scan deadline.
        // This hook is private to scanner tests; entry delays never sleep.
        #[cfg(test)]
        if !self.info_delay.is_zero() {
            std::thread::sleep(self.info_delay);
        }
        #[cfg(test)]
        if let Some((count, errno)) = self.info_reply {
            validate_info(count, errno)?;
            // Injected replies never claim initialized fields.
            return if errno == libc::ESRCH {
                Ok(None)
            } else {
                Err(UnverifiedReason::InfoFailed)
            };
        }
        pid_info(pid)
    }

    #[cfg(test)]
    pub(in crate::supervisor) fn inject_list_error(&mut self, errno: i32) {
        self.list_reply = Some((0, errno));
    }

    #[cfg(test)]
    pub(in crate::supervisor) fn inject_short_info(&mut self) {
        self.info_reply = Some((size_of::<libc::proc_bsdinfo>() as i32 - 1, 0));
    }

    #[cfg(test)]
    pub(in crate::supervisor) fn inject_full_lists(&mut self) {
        self.full_lists = true;
    }
}

#[cfg(test)]
pub(in crate::supervisor) fn test_members(pgid: i32) -> Vec<libc::proc_bsdinfo> {
    let mut pids = vec![0; PID_CAP];
    let (count, errno) = list_pids(pgid as u32, &mut pids);
    let count = validate_list(count, errno, pids.len()).unwrap();
    assert!(count < pids.len());
    pids[..count]
        .iter()
        .filter(|pid| **pid > 0)
        .filter_map(|pid| pid_info(*pid).unwrap())
        .filter(|info| info.pbi_pgid == pgid as u32)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan() -> CleanupScan {
        cleanup_scan(123, None).unwrap()
    }

    #[test]
    fn malformed_counts_errors_and_empty_success_are_distinct() {
        let bytes = INITIAL_PIDS * size_of::<libc::pid_t>();
        for count in [-1, 1, bytes as i32 + 1, bytes as i32 + 4] {
            assert_eq!(
                validate_list(count, 0, INITIAL_PIDS),
                Err(UnverifiedReason::Malformed)
            );
        }
        for count in [0, 4, bytes as i32] {
            assert_eq!(
                validate_list(count, libc::EACCES, INITIAL_PIDS),
                Err(UnverifiedReason::Io)
            );
        }
        assert_eq!(
            validate_list(0, libc::ENOSYS, INITIAL_PIDS),
            Err(UnverifiedReason::Unsupported)
        );
        let mut scan = scan();
        scan.list_reply = Some((0, 0));
        assert_eq!(
            scan.poll(Instant::now() + Duration::from_secs(1), None),
            Some(Cleanup::Confirmed)
        );
    }

    #[test]
    fn info_requires_exact_size_and_clean_errno_except_for_disappearance() {
        let size = size_of::<libc::proc_bsdinfo>() as i32;
        for count in [-1, 0, size - 1, size + 1] {
            assert_eq!(validate_info(count, 0), Err(UnverifiedReason::InfoFailed));
        }
        assert_eq!(
            validate_info(size, libc::EACCES),
            Err(UnverifiedReason::InfoFailed)
        );
        assert_eq!(validate_info(size, 0), Ok(()));
        assert_eq!(validate_info(0, libc::ESRCH), Ok(()));
    }

    #[test]
    fn full_buffers_grow_without_reallocation_and_stop_at_cap() {
        let mut scan = scan();
        scan.inject_full_lists();
        let pointer = scan.buffer.as_ptr();
        let deadline = Instant::now() + Duration::from_secs(1);
        for expected in [1024, 2048, 4096, 8192, 16384, 32768, PID_CAP] {
            assert_eq!(scan.poll(deadline, None), None);
            assert_eq!(scan.window, expected);
            assert_eq!(scan.buffer.as_ptr(), pointer);
        }
        assert_eq!(
            scan.poll(deadline, None),
            Some(Cleanup::Unverified {
                reason: UnverifiedReason::Truncated
            })
        );
    }

    #[test]
    fn delayed_entries_yield_and_deadlines_never_report_partial_counts() {
        for reap_first in [false, true] {
            let mut scan = scan();
            scan.list_reply = Some((size_of::<libc::pid_t>() as i32, 0));
            scan.buffer[0] = 123;
            scan.set_entry_delay(Duration::from_secs(1));
            let now = Instant::now();
            let early = now + Duration::from_millis(20);
            let later = now + Duration::from_secs(2);
            let (cleanup, reap, reason) = if reap_first {
                (later, early, UnverifiedReason::ReapDeadline)
            } else {
                (early, later, UnverifiedReason::ScanIncomplete)
            };
            assert_eq!(scan.poll(cleanup, Some(reap)), None);
            assert!(now.elapsed() < Duration::from_millis(20));
            // A transfer retains the buffer, index and pending entry, without sleeping.
            let mut transferred = scan;
            std::thread::sleep(early.saturating_duration_since(Instant::now()));
            assert_eq!(
                transferred.poll(cleanup, Some(reap)),
                Some(Cleanup::Unverified { reason })
            );
        }
    }

    #[test]
    fn deadline_during_full_list_retries_cannot_confirm() {
        let mut scan = scan();
        scan.inject_full_lists();
        assert_eq!(
            scan.poll(Instant::now() + Duration::from_secs(1), None),
            None
        );
        assert_eq!(
            scan.poll(Instant::now(), None),
            Some(Cleanup::Unverified {
                reason: UnverifiedReason::ScanIncomplete
            })
        );
    }

    #[test]
    fn inspection_is_bounded_and_an_esrch_entry_is_gone() {
        let mut scan = scan();
        scan.list_reply = Some((((POLL_ENTRIES + 1) * size_of::<libc::pid_t>()) as i32, 0));
        scan.buffer.fill(456);
        scan.info_reply = Some((0, libc::ESRCH));
        let deadline = Instant::now() + Duration::from_secs(1);
        assert_eq!(scan.poll(deadline, None), None);
        assert!(scan.index <= POLL_ENTRIES);
        let result = loop {
            if let Some(result) = scan.poll(deadline, None) {
                break result;
            }
        };
        assert_eq!(result, Cleanup::Confirmed);
    }

    #[test]
    fn completed_live_scan_at_deadline_differs_from_no_completed_scan() {
        let mut scan = scan();
        assert_eq!(
            scan.poll(Instant::now(), None),
            Some(Cleanup::Unverified {
                reason: UnverifiedReason::ScanIncomplete
            })
        );
        scan.last_survivors = Some(1);
        assert_eq!(
            scan.poll(Instant::now(), None),
            Some(Cleanup::Incomplete { survivors: 1 })
        );
        let now = Instant::now();
        assert_eq!(
            scan.poll(now, Some(now)),
            Some(Cleanup::Unverified {
                reason: UnverifiedReason::ReapDeadline
            })
        );
    }
    #[test]
    fn newer_scan_cut_by_the_cleanup_deadline_keeps_the_last_live_count() {
        // SAFETY: getpid only queries the current identity.
        let pid = unsafe { libc::getpid() };
        let mut scan = scan();
        scan.pgid = pid as u32 + 1;
        scan.buffer[0] = pid;
        scan.list_reply = Some((size_of::<libc::pid_t>() as i32, 0));
        scan.last_survivors = Some(2);
        scan.info_delay = Duration::from_millis(50);
        let cleanup = Instant::now() + Duration::from_millis(10);
        // The deadline passes inside pidinfo; the interrupted rescan cannot
        // replace the earlier observation of live members with Unverified.
        assert_eq!(
            scan.poll(cleanup, None),
            Some(Cleanup::Incomplete { survivors: 2 })
        );
    }

    #[test]
    fn inspection_rechecks_membership_instead_of_trusting_enumeration() {
        // Enumerate this real process under an intentionally different group.
        // Only rechecking pbi_pgid can exclude this live pid.
        // SAFETY: both identity queries take no pointers or change process state.
        let (pid, pgid) = unsafe { (libc::getpid(), libc::getpgrp()) };
        let mut scan = scan();
        scan.pgid = (pgid as u32 + 1).max(pid as u32 + 1);
        scan.buffer[0] = pid;
        scan.list_reply = Some((size_of::<libc::pid_t>() as i32, 0));
        let deadline = Instant::now() + Duration::from_secs(1);
        let result = loop {
            if let Some(result) = scan.poll(deadline, None) {
                break result;
            }
        };
        assert_eq!(result, Cleanup::Confirmed);
    }

    #[test]
    fn native_wrappers_clear_stale_errno_before_successful_calls() {
        // SAFETY: getpid/getpgrp only query identities; errno is thread-local.
        let (pid, pgid) = unsafe {
            let identity = (libc::getpid(), libc::getpgrp());
            *libc::__error() = libc::EACCES;
            identity
        };
        let mut buffer = [0; INITIAL_PIDS];
        let (count, errno) = list_pids(pgid as u32, &mut buffer);
        assert_eq!(errno, 0);
        assert!(validate_list(count, errno, buffer.len()).is_ok());
        // SAFETY: only this thread's errno is changed; pid_info must clear it.
        unsafe {
            *libc::__error() = libc::EACCES;
        }
        let info = pid_info(pid).unwrap().unwrap();
        let (actual_pid, actual_group) = (info.pbi_pid, info.pbi_pgid);
        assert_eq!(actual_pid, pid as u32);
        assert_eq!(actual_group, pgid as u32);
    }

    #[test]
    fn reap_cutoff_during_pidinfo_retains_the_unaccepted_entry_for_transfer() {
        // SAFETY: getpid only queries the current identity.
        let pid = unsafe { libc::getpid() };
        let mut scan = scan();
        scan.pgid = pid as u32 + 1;
        scan.buffer[0] = pid;
        scan.list_reply = Some((size_of::<libc::pid_t>() as i32, 0));
        scan.info_delay = Duration::from_millis(100);
        let cleanup = Instant::now() + Duration::from_secs(2);
        let reap = Instant::now() + Duration::from_millis(20);
        let result = loop {
            if let Some(result) = scan.poll(cleanup, Some(reap)) {
                break result;
            }
        };
        assert_eq!(
            result,
            Cleanup::Unverified {
                reason: UnverifiedReason::ReapDeadline,
            }
        );
        assert_eq!(scan.index, 0);
        let mut transferred = scan;
        transferred.info_delay = Duration::ZERO;
        transferred.inject_short_info();
        let result = loop {
            if let Some(result) = transferred.poll(cleanup, None) {
                break result;
            }
        };
        // Skipping the interrupted entry would incorrectly confirm this scan.
        assert_eq!(
            result,
            Cleanup::Unverified {
                reason: UnverifiedReason::InfoFailed
            }
        );
    }
}
