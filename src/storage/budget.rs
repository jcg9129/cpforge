//! Exact in-process accounting of tool-created logical file lengths (§6.4).
//!
//! Every producer reserves before allocating, including publication temporary
//! files and copies. Remove a file successfully before releasing its retained
//! length; a failed removal releases nothing. Hard links and reflinks count at
//! full logical length for each name. Child-created output is outside this quota.
//!
//! The supervisor can reserve each file stream's full clipped budget and the
//! fixed 1 MiB metadata allowance (§5.4) through this same interface. Sum budgets
//! with checked arithmetic before reserving, or drop all earlier reservations
//! if a later reservation fails. Transfer reservations together with stalled
//! writers and owner clones; only commit actual logical lengths at resources
//! released, after the last write returns. A lease alone reserves no headroom.

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BudgetUsage {
    pub retained: u64,
    pub reserved: u64,
}

/// Share one `Arc<JobBudget>` with every owner and producer of a job's files.
/// After registration, file producers keep a live job owner as well as their
/// reservation. Releasing the last owner ends retained-byte accounting.
#[derive(Debug)]
pub struct JobBudget {
    limit: u64,
    usage: Mutex<BudgetUsage>,
}

impl JobBudget {
    pub fn new(limit: u64) -> Self {
        Self {
            limit,
            usage: Mutex::new(BudgetUsage::default()),
        }
    }

    pub fn limit(&self) -> u64 {
        self.limit
    }

    /// An atomic snapshot; retained + reserved never exceeds `limit()`.
    pub fn usage(&self) -> BudgetUsage {
        *self.lock()
    }

    pub fn reserve(self: &Arc<Self>, bytes: u64) -> Result<Reservation, BudgetExceeded> {
        let mut usage = self.lock();
        let total = usage
            .retained
            .checked_add(usage.reserved)
            .and_then(|total| total.checked_add(bytes));
        if !matches!(total, Some(total) if total <= self.limit) {
            return Err(BudgetExceeded {
                limit: self.limit,
                usage: *usage,
                requested: bytes,
            });
        }
        // The checked total also proves that this addition cannot overflow.
        usage.reserved += bytes;
        Ok(Reservation {
            budget: Arc::clone(self),
            bytes,
        })
    }

    /// Call only after successful removal. Underflow is a programming error:
    /// return an error without changing accounting, rather than saturating.
    pub fn release_retained(&self, bytes: u64) -> Result<(), ReleaseError> {
        let mut usage = self.lock();
        usage.retained = usage.retained.checked_sub(bytes).ok_or(ReleaseError {
            retained: usage.retained,
            requested: bytes,
        })?;
        Ok(())
    }

    pub(crate) fn owner_released(&self) {
        self.lock().retained = 0;
    }

    fn lock(&self) -> MutexGuard<'_, BudgetUsage> {
        self.usage
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

/// An owned, transferable allowance. Drop returns an uncommitted allowance;
/// do that only before writing or after removing any partial files. After writes
/// (even failed writes), commit every logical byte still present on disk.
#[derive(Debug)]
#[must_use = "keep the reservation until writes finish, then commit actual lengths"]
pub struct Reservation {
    budget: Arc<JobBudget>,
    bytes: u64,
}

impl Reservation {
    /// Transfer part of an existing allowance without reserving twice.
    pub fn split(&mut self, bytes: u64) -> Option<Self> {
        self.bytes = self.bytes.checked_sub(bytes)?;
        Some(Self {
            budget: Arc::clone(&self.budget),
            bytes,
        })
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Convert actual bytes to retained and return unused headroom atomically.
    /// An over-commit is a programming error and leaves this reservation intact
    /// so the caller can remove the excess and retry. No allocation may exceed
    /// the original allowance, even temporarily.
    pub fn commit(&mut self, actual_bytes: u64) -> Result<(), CommitError> {
        if actual_bytes > self.bytes {
            return Err(CommitError {
                reserved: self.bytes,
                actual: actual_bytes,
            });
        }
        let mut usage = self.budget.lock();
        // actual <= this reservation; the invariant proves addition is safe.
        usage.reserved -= self.bytes;
        usage.retained += actual_bytes;
        self.bytes = 0;
        Ok(())
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.budget.lock().reserved -= self.bytes;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BudgetExceeded {
    pub limit: u64,
    pub usage: BudgetUsage,
    pub requested: u64,
}

impl fmt::Display for BudgetExceeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "job byte budget exceeded (limit {}, retained {}, reserved {}, requested {})",
            self.limit, self.usage.retained, self.usage.reserved, self.requested
        )
    }
}

impl std::error::Error for BudgetExceeded {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommitError {
    pub reserved: u64,
    pub actual: u64,
}

impl fmt::Display for CommitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "cannot commit {} bytes against {} reserved bytes",
            self.actual, self.reserved
        )
    }
}

impl std::error::Error for CommitError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReleaseError {
    pub retained: u64,
    pub requested: u64,
}

impl fmt::Display for ReleaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "cannot release {} bytes from {} retained bytes",
            self.requested, self.retained
        )
    }
}

impl std::error::Error for ReleaseError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_usage(budget: &JobBudget, retained: u64, reserved: u64) {
        assert_eq!(budget.usage(), BudgetUsage { retained, reserved });
        assert!(retained.checked_add(reserved).unwrap() <= budget.limit());
    }

    #[test]
    fn reserve_commit_drop_and_release_share_one_invariant() {
        let budget = Arc::new(JobBudget::new(100));
        let clone = Arc::clone(&budget);
        let mut first = budget.reserve(60).unwrap();
        let second = clone.reserve(40).unwrap();
        assert_usage(&budget, 0, 100);
        assert_eq!(second.bytes(), 40);
        assert!(budget.reserve(1).is_err());
        first.commit(35).unwrap();
        assert_usage(&budget, 35, 40);
        drop(first);
        assert_usage(&budget, 35, 40);
        drop(second);
        assert_usage(&budget, 35, 0);
        assert_eq!(
            budget.release_retained(36),
            Err(ReleaseError {
                retained: 35,
                requested: 36
            })
        );
        assert_usage(&budget, 35, 0);
        budget.release_retained(35).unwrap();
        assert_usage(&budget, 0, 0);
        let mut zero = clone.reserve(0).unwrap();
        zero.commit(0).unwrap();
        assert_usage(&budget, 0, 0);
    }

    #[test]
    fn overcommit_preserves_allowance_for_cleanup_and_retry() {
        let budget = Arc::new(JobBudget::new(100));
        let mut reservation = budget.reserve(30).unwrap();
        let other = budget.reserve(70).unwrap();
        assert_eq!(
            reservation.commit(31),
            Err(CommitError {
                reserved: 30,
                actual: 31
            })
        );
        assert_eq!(reservation.bytes(), 30);
        assert_usage(&budget, 0, 100);
        reservation.commit(29).unwrap();
        assert_usage(&budget, 29, 70);
        assert!(reservation.commit(1).is_err());
        assert_usage(&budget, 29, 70);
        drop(other);
        assert_usage(&budget, 29, 0);
    }

    #[test]
    fn checked_arithmetic_refuses_overflow_without_changing_counters() {
        let budget = Arc::new(JobBudget::new(u64::MAX));
        let mut full = budget.reserve(u64::MAX).unwrap();
        assert!(budget.reserve(1).is_err());
        full.commit(u64::MAX - 1).unwrap();
        let last = budget.reserve(1).unwrap();
        assert!(budget.reserve(1).is_err());
        assert!(budget.reserve(u64::MAX).is_err());
        assert_usage(&budget, u64::MAX - 1, 1);
        drop(last);
        budget.release_retained(u64::MAX - 1).unwrap();
        assert_usage(&budget, 0, 0);
        assert!(Arc::new(JobBudget::new(0)).reserve(1).is_err());
    }

    #[test]
    fn concurrent_reservations_cannot_overbook() {
        let budget = Arc::new(JobBudget::new(7));
        let handles: Vec<_> = (0..16)
            .map(|_| {
                let budget = Arc::clone(&budget);
                std::thread::spawn(move || budget.reserve(1))
            })
            .collect();
        let reservations: Vec<_> = handles
            .into_iter()
            .filter_map(|handle| handle.join().unwrap().ok())
            .collect();
        assert_eq!(reservations.len(), 7);
        assert_usage(&budget, 0, 7);
        drop(reservations);
        assert_usage(&budget, 0, 0);
    }

    #[test]
    fn split_transfers_reserved_headroom_without_double_counting() {
        let budget = Arc::new(JobBudget::new(100));
        let mut parent = budget.reserve(100).unwrap();
        let mut child = parent.split(60).unwrap();
        assert_eq!(parent.bytes(), 40);
        assert_eq!(child.bytes(), 60);
        assert!(parent.split(41).is_none());
        assert_eq!(
            budget.usage(),
            BudgetUsage {
                retained: 0,
                reserved: 100
            }
        );
        child.commit(20).unwrap();
        drop(parent);
        assert_eq!(
            budget.usage(),
            BudgetUsage {
                retained: 20,
                reserved: 0
            }
        );
    }
}
