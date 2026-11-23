//! Memory credits are unrelated to disk reservations. FIFO grants are made at
//! write return, even if the producer is not currently scheduled.
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::sync::Notify;

use super::clock::Clock;
use crate::jobs::engine::PersistError;
use crate::supervisor::CancelToken;

pub(super) const CAPACITY: usize = 2 * 1024 * 1024;
pub(super) const OVERHEAD: usize = 8 * 1024;
const WAIT_LIMIT: Duration = Duration::from_secs(1);

#[derive(Debug)]
struct Waiter {
    charge: usize,
    call: Instant,
    cancel: CancelToken,
    result: Mutex<Option<Result<(), PersistError>>>,
}
#[derive(Debug)]
struct State {
    used: usize,
    peak: usize,
    nonblocking: bool,
    closed_at: Option<Instant>,
    budget: Duration,
    waited: Duration,
    since: Option<Instant>,
    waiters: VecDeque<Arc<Waiter>>,
    publications: BTreeMap<u64, Instant>,
    next: u64,
    #[cfg(any(test, feature = "storage-test-hooks"))]
    last_release: Option<Instant>,
    #[cfg(any(test, feature = "storage-test-hooks"))]
    cycles: Vec<Duration>,
    #[cfg(any(test, feature = "storage-test-hooks"))]
    last_write: Option<std::path::PathBuf>,
    #[cfg(any(test, feature = "storage-test-hooks"))]
    record_idle_at_enqueue: Vec<bool>,
}
impl State {
    fn account(&mut self, now: Instant) {
        if let Some(since) = self.since.replace(now) {
            self.waited += now.saturating_duration_since(since);
        }
        if self.waiters.is_empty() {
            self.since = None;
        }
    }

    fn refuse(&mut self, error: PersistError) {
        for waiter in self.waiters.drain(..) {
            *waiter.result.lock().unwrap() = Some(Err(error.clone()));
        }
        self.since = None;
    }

    fn update(&mut self, now: Instant, closed: bool) {
        // Charge only the union of waits, capped at the first policy limit.
        let boundary = self.waiters.front().map(|first| {
            (first.call + WAIT_LIMIT)
                .min(self.since.unwrap_or(now) + self.budget.saturating_sub(self.waited))
        });
        let closed_at = if closed {
            Some(*self.closed_at.get_or_insert(now))
        } else {
            None
        };
        let effective = boundary
            .into_iter()
            .chain(closed_at)
            .fold(now, Instant::min);
        self.account(effective);
        if closed_at.is_some_and(|at| boundary.is_none_or(|limit| at <= limit)) {
            self.refuse(PersistError::WorkerUnavailable);
        } else if self.waited >= self.budget
            || self
                .waiters
                .front()
                .is_some_and(|w| now >= w.call + WAIT_LIMIT)
        {
            self.nonblocking = true;
            self.refuse(PersistError::QueueFull);
        } else if closed {
            self.refuse(PersistError::WorkerUnavailable);
        } else if self
            .waiters
            .front()
            .is_some_and(|w| w.cancel.is_cancelled())
        {
            self.refuse(PersistError::Cancelled);
        } else {
            while self
                .waiters
                .front()
                .is_some_and(|w| w.charge <= CAPACITY - self.used)
            {
                let waiter = self.waiters.pop_front().unwrap();
                self.used += waiter.charge;
                self.peak = self.peak.max(self.used);
                *waiter.result.lock().unwrap() = Some(Ok(()));
            }
            if self.waiters.is_empty() {
                self.since = None;
            }
        }
    }
}

#[derive(Debug)]
pub(crate) struct Pool {
    state: Mutex<State>,
    pub(super) changed: Notify,
    #[cfg(test)]
    pub(crate) waiting: Notify,
    pub(super) unavailable: Arc<AtomicBool>,
    pub(super) timed_out: AtomicBool,
    clock: Clock,
}
impl Pool {
    pub(super) fn new(clock: Clock) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                used: 0,
                peak: 0,
                nonblocking: false,
                closed_at: None,
                budget: Duration::from_secs(10),
                waited: Duration::ZERO,
                since: None,
                waiters: VecDeque::new(),
                publications: BTreeMap::new(),
                next: 0,
                #[cfg(any(test, feature = "storage-test-hooks"))]
                last_release: None,
                #[cfg(any(test, feature = "storage-test-hooks"))]
                cycles: Vec::new(),
                #[cfg(any(test, feature = "storage-test-hooks"))]
                last_write: None,
                #[cfg(any(test, feature = "storage-test-hooks"))]
                record_idle_at_enqueue: Vec::new(),
            }),
            changed: Notify::new(),
            #[cfg(test)]
            waiting: Notify::new(),
            unavailable: Arc::new(AtomicBool::new(false)),
            timed_out: AtomicBool::new(false),
            clock,
        })
    }
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub(super) fn configure(&self, planned: u32) {
        self.lock().budget =
            Duration::from_secs(10) + Duration::from_millis(12 * u64::from(planned));
    }
    pub(super) fn close(&self) {
        self.unavailable.store(true, Ordering::Release);
        let mut state = self.lock();
        let now = self.clock.now();
        state.closed_at.get_or_insert(now);
        state.update(now, true);
        drop(state);
        self.changed.notify_waiters();
    }
    pub(super) fn timeout(&self) {
        self.timed_out.store(true, Ordering::Release);
        let mut state = self.lock();
        let now = self.clock.now();
        let deadline = state
            .publications
            .values()
            .min()
            .copied()
            .unwrap_or(now)
            .min(now);
        state.closed_at = Some(state.closed_at.map_or(deadline, |at| at.min(deadline)));
        self.unavailable.store(true, Ordering::Release);
        state.update(now, true);
        drop(state);
        self.changed.notify_waiters();
    }
    fn release(&self, charge: usize) {
        let mut state = self.lock();
        #[cfg(any(test, feature = "storage-test-hooks"))]
        if let Some(last) = state.last_release {
            state
                .cycles
                .push(self.clock.now().saturating_duration_since(last));
        }
        state.used -= charge;
        state.update(self.clock.now(), self.unavailable.load(Ordering::Acquire));
        #[cfg(any(test, feature = "storage-test-hooks"))]
        {
            state.last_release = (state.used > 0).then(|| self.clock.now());
        }
        drop(state);
        self.changed.notify_waiters();
    }
    #[cfg(any(test, feature = "storage-test-hooks"))]
    pub(super) fn record_enqueue(&self) {
        let mut state = self.lock();
        let idle = state.used == 0 && state.publications.is_empty();
        state.record_idle_at_enqueue.push(idle);
    }
    #[cfg(any(test, feature = "storage-test-hooks"))]
    pub(super) fn written(&self, path: &std::path::Path) {
        self.lock().last_write = Some(path.to_owned());
    }
    #[cfg(any(test, feature = "storage-test-hooks"))]
    pub(super) fn stats(&self) -> super::PersistenceStats {
        let state = self.lock();
        super::PersistenceStats {
            peak_credits: state.peak,
            credit_wait: state.waited,
            service_cycles: state.cycles.clone(),
            last_write: state.last_write.clone(),
            settlement_elapsed: None,
            record_idle_at_enqueue: state.record_idle_at_enqueue.clone(),
        }
    }
    fn refresh(&self, state: &mut State, now: Instant) {
        if let Some(deadline) = state
            .publications
            .values()
            .min()
            .copied()
            .filter(|&d| now >= d)
        {
            state.closed_at = Some(state.closed_at.map_or(deadline, |at| at.min(deadline)));
            self.timed_out.store(true, Ordering::Release);
            self.unavailable.store(true, Ordering::Release);
        }
        state.update(now, self.unavailable.load(Ordering::Acquire));
    }
    pub(super) fn publication(self: &Arc<Self>, deadline: Instant) -> Publication {
        let mut state = self.lock();
        let id = state.next;
        state.next += 1;
        state.publications.insert(id, deadline);
        Publication {
            pool: self.clone(),
            id,
            deadline,
        }
    }
    pub(crate) async fn acquire(
        self: &Arc<Self>,
        charge: usize,
        call: Instant,
        cancel: &CancelToken,
    ) -> Result<Credit, PersistError> {
        let waiter = Arc::new(Waiter {
            charge,
            call,
            cancel: cancel.clone(),
            result: Mutex::new(None),
        });
        {
            let mut state = self.lock();
            let now = self.clock.now();
            self.refresh(&mut state, now);
            if self.unavailable.load(Ordering::Acquire) {
                return Err(PersistError::WorkerUnavailable);
            }
            if state.waiters.is_empty() && charge <= CAPACITY - state.used {
                state.used += charge;
                state.peak = state.peak.max(state.used);
                return Ok(Credit {
                    pool: self.clone(),
                    charge,
                });
            }
            if cancel.is_cancelled() {
                return Err(PersistError::Cancelled);
            }
            if state.nonblocking {
                return Err(PersistError::QueueFull);
            }
            if state.waiters.is_empty() {
                state.since = Some(now);
            }
            state.waiters.push_back(waiter.clone());
        }
        #[cfg(test)]
        self.waiting.notify_one();
        let mut waiting = Waiting {
            pool: self.clone(),
            waiter: waiter.clone(),
            taken: false,
        };
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let deadline = {
                let mut state = self.lock();
                let now = self.clock.now();
                self.refresh(&mut state, now);
                if cancel.is_cancelled() {
                    state.refuse(PersistError::Cancelled);
                }
                if let Some(result) = waiter.result.lock().unwrap().take() {
                    waiting.taken = true;
                    return result.map(|()| Credit {
                        pool: self.clone(),
                        charge,
                    });
                }
                let budget_end = now + state.budget.saturating_sub(state.waited);
                let limit = state.waiters.front().unwrap().call + WAIT_LIMIT;
                state
                    .publications
                    .values()
                    .min()
                    .map_or(limit.min(budget_end), |p| (*p).min(limit).min(budget_end))
            };
            tokio::select! {
                _ = changed => {},
                _ = self.clock.until(deadline) => {},
                _ = cancel.cancelled() => {},
            }
        }
    }
}

#[derive(Debug)]
pub(super) struct CloseOnExit(pub(super) Arc<Pool>);
impl Drop for CloseOnExit {
    fn drop(&mut self) {
        self.0.close();
    }
}

// Dropping an enqueue future must neither strand its FIFO position nor leak a
// grant made while it was descheduled.
struct Waiting {
    pool: Arc<Pool>,
    waiter: Arc<Waiter>,
    taken: bool,
}
impl Drop for Waiting {
    fn drop(&mut self) {
        if self.taken {
            return;
        }
        let mut state = self.pool.lock();
        self.pool.refresh(&mut state, self.pool.clock.now());
        state.waiters.retain(|w| !Arc::ptr_eq(w, &self.waiter));
        if matches!(*self.waiter.result.lock().unwrap(), Some(Ok(()))) {
            state.used -= self.waiter.charge;
        }
        state.update(
            self.pool.clock.now(),
            self.pool.unavailable.load(Ordering::Acquire),
        );
        drop(state);
        self.pool.changed.notify_waiters();
    }
}

#[derive(Debug)]
pub(crate) struct Credit {
    pool: Arc<Pool>,
    charge: usize,
}
impl Drop for Credit {
    fn drop(&mut self) {
        self.pool.release(self.charge);
    }
}

// This token also lets a credit waiter watch the live publication deadlines,
// rather than a stale deadline for an already completed write.
#[derive(Debug)]
pub(super) struct Publication {
    pool: Arc<Pool>,
    id: u64,
    deadline: Instant,
}
impl Publication {
    pub(super) fn returned(&mut self) -> bool {
        let mut state = self.pool.lock();
        let now = self.pool.clock.now();
        let expired = now >= self.deadline;
        let late = expired || self.pool.unavailable.load(Ordering::Acquire);
        if expired {
            state.closed_at = Some(
                state
                    .closed_at
                    .map_or(self.deadline, |at| at.min(self.deadline)),
            );
            self.pool.timed_out.store(true, Ordering::Release);
        }
        if late {
            self.pool.unavailable.store(true, Ordering::Release);
        }
        state.publications.remove(&self.id);
        state.update(now, late);
        drop(state);
        self.pool.changed.notify_waiters();
        late
    }
}
impl Drop for Publication {
    fn drop(&mut self) {
        self.pool.lock().publications.remove(&self.id);
        self.pool.changed.notify_waiters();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll, Waker};

    fn poll<T>(future: Pin<&mut (impl Future<Output = T> + ?Sized)>) -> Poll<T> {
        future.poll(&mut Context::from_waker(Waker::noop()))
    }
    fn acquire(pool: &Arc<Pool>, charge: usize, cancel: &CancelToken) -> Credit {
        let mut future = Box::pin(pool.acquire(charge, pool.clock.now(), cancel));
        match poll(future.as_mut()) {
            Poll::Ready(Ok(credit)) => credit,
            other => panic!("immediate grant expected: {other:?}"),
        }
    }
    fn failed<T: std::fmt::Debug>(result: Poll<Result<T, PersistError>>, error: PersistError) {
        assert!(
            matches!(result, Poll::Ready(Err(ref actual)) if *actual == error),
            "{result:?}"
        );
    }

    #[test]
    fn fifo_charges_and_cancellation_do_not_spend_disk_reservations() {
        let clock = Clock::controlled();
        let pool = Pool::new(clock.clone());
        let cancel = CancelToken::new();
        let budget = Arc::new(crate::storage::budget::JobBudget::new(100));
        let reservation = budget.reserve(100).unwrap();
        let full = acquire(&pool, CAPACITY - OVERHEAD, &cancel);
        let call = clock.now();
        let mut large = Box::pin(pool.acquire(2 * OVERHEAD, call, &cancel));
        let mut small = Box::pin(pool.acquire(OVERHEAD, call, &cancel));
        assert!(poll(large.as_mut()).is_pending());
        assert!(
            poll(small.as_mut()).is_pending(),
            "a small request cannot overtake"
        );
        clock.advance(Duration::from_millis(500));
        cancel.cancel();
        // The worker notices intent even if neither producer has been polled.
        drop(full);
        failed(poll(large.as_mut()), PersistError::Cancelled);
        failed(poll(small.as_mut()), PersistError::Cancelled);
        assert_eq!(pool.lock().waited, Duration::from_millis(500));
        assert_eq!(budget.usage().reserved, 100);
        let evidence = acquire(&pool, OVERHEAD, &cancel);
        assert_eq!(pool.lock().used, OVERHEAD);
        drop(reservation);
        assert_eq!(
            pool.lock().used,
            OVERHEAD,
            "disk headroom is not queue capacity"
        );
        drop(evidence);
        let full = acquire(&pool, CAPACITY, &cancel);
        let mut refused = Box::pin(pool.acquire(OVERHEAD, clock.now(), &cancel));
        failed(poll(refused.as_mut()), PersistError::Cancelled);
        drop(full);
        assert_eq!(pool.lock().used, 0);
    }

    #[test]
    fn abandoned_waiter_returns_a_grant_and_does_not_block_followers() {
        let clock = Clock::controlled();
        let pool = Pool::new(clock);
        let cancel = CancelToken::new();
        let full = acquire(&pool, CAPACITY, &cancel);
        let mut first = Box::pin(pool.acquire(OVERHEAD, pool.clock.now(), &cancel));
        let mut second = Box::pin(pool.acquire(OVERHEAD, pool.clock.now(), &cancel));
        assert!(poll(first.as_mut()).is_pending());
        assert!(poll(second.as_mut()).is_pending());
        drop(full);
        drop(first); // granted, but not yet observed
        assert_eq!(pool.lock().used, OVERHEAD);
        let Poll::Ready(Ok(credit)) = poll(second.as_mut()) else {
            panic!("follower stuck");
        };
        drop(credit);
        assert_eq!(pool.lock().used, 0);
    }

    #[test]
    fn write_and_between_write_pauses_of_1200ms_switch_to_nonblocking_at_one_second() {
        for between_writes in [false, true] {
            let clock = Clock::controlled();
            let pool = Pool::new(clock.clone());
            let cancel = CancelToken::new();
            let mut credits = VecDeque::new();
            for _ in 0..256 {
                credits.push_back(acquire(&pool, OVERHEAD, &cancel));
            }
            if between_writes {
                drop(credits.pop_front()); // one return before the pause
                credits.push_back(acquire(&pool, OVERHEAD, &cancel));
            }
            let call = clock.now();
            let mut waiting = Box::pin(pool.acquire(OVERHEAD, call, &cancel));
            assert!(poll(waiting.as_mut()).is_pending());
            clock.advance(Duration::from_millis(999));
            assert!(poll(waiting.as_mut()).is_pending());
            clock.advance(Duration::from_millis(1));
            failed(poll(waiting.as_mut()), PersistError::QueueFull);
            assert_eq!(clock.now(), call + WAIT_LIMIT);
            let mut later = Box::pin(pool.acquire(OVERHEAD, clock.now(), &cancel));
            failed(poll(later.as_mut()), PersistError::QueueFull);
            clock.advance(Duration::from_millis(200));
            drop(credits.pop_front());
            drop(acquire(&pool, OVERHEAD, &cancel)); // free credits still usable
            assert_eq!(pool.lock().waited, WAIT_LIMIT);
            assert!(!pool.unavailable.load(Ordering::Acquire));
        }
    }

    #[test]
    fn exited_worker_wakes_credit_waiters_without_fabricating_a_timeout() {
        let clock = Clock::controlled();
        let pool = Pool::new(clock);
        let cancel = CancelToken::new();
        let full = acquire(&pool, CAPACITY, &cancel);
        let mut waiting = Box::pin(pool.acquire(OVERHEAD, pool.clock.now(), &cancel));
        assert!(poll(waiting.as_mut()).is_pending());
        drop(CloseOnExit(pool.clone()));
        failed(poll(waiting.as_mut()), PersistError::WorkerUnavailable);
        assert!(!pool.timed_out.load(Ordering::Acquire));
        drop(full);
    }

    #[test]
    fn live_publication_deadline_closes_waits_and_late_returns_never_reissue_credits() {
        let clock = Clock::controlled();
        let pool = Pool::new(clock.clone());
        let cancel = CancelToken::new();
        // This write's call was four seconds ago. Its deadline wins over the
        // new wait's one-second limit at the same instant.
        let mut publication = pool.publication(clock.now() + WAIT_LIMIT);
        let full = acquire(&pool, CAPACITY, &cancel);
        let mut waiting = Box::pin(pool.acquire(OVERHEAD, clock.now(), &cancel));
        assert!(poll(waiting.as_mut()).is_pending());
        clock.advance(WAIT_LIMIT);
        failed(poll(waiting.as_mut()), PersistError::WorkerUnavailable);
        assert!(publication.returned());
        drop(full);
        assert_eq!(pool.lock().used, 0);
        let mut later = Box::pin(pool.acquire(OVERHEAD, clock.now(), &cancel));
        failed(poll(later.as_mut()), PersistError::WorkerUnavailable);
    }

    #[test]
    fn completed_publication_does_not_leave_a_stale_wait_deadline() {
        let clock = Clock::controlled();
        let pool = Pool::new(clock.clone());
        let cancel = CancelToken::new();
        let full = acquire(&pool, CAPACITY, &cancel);
        let mut publication = pool.publication(clock.now() + Duration::from_millis(100));
        let mut waiting = Box::pin(pool.acquire(OVERHEAD, clock.now(), &cancel));
        assert!(poll(waiting.as_mut()).is_pending());
        clock.advance(Duration::from_millis(99));
        assert!(!publication.returned());
        clock.advance(Duration::from_millis(401));
        assert!(poll(waiting.as_mut()).is_pending());
        drop(full);
        let Poll::Ready(Ok(credit)) = poll(waiting.as_mut()) else {
            panic!("grant missing");
        };
        assert!(!pool.unavailable.load(Ordering::Acquire));
        drop(credit);
    }

    struct Accepted {
        credit: Credit,
        publication: Publication,
        call: Instant,
    }
    type Enqueue = Pin<Box<dyn Future<Output = Result<Credit, PersistError>>>>;

    // Five continuously active producers, 3 artefacts per execution. The
    // scripted writer covers the entire release-to-release cycle. Only this
    // driver advances time; encoding, scheduling and assertions consume none.
    fn script(cycle_ms: u64, pause: Option<(bool, Duration)>) {
        let clock = Clock::controlled();
        let pool = Pool::new(clock.clone());
        pool.configure(3000);
        assert_eq!(pool.lock().budget, Duration::from_secs(46));
        let cancel = CancelToken::new();
        let mut writing = VecDeque::<Accepted>::new();
        let mut producers: Vec<Option<(Enqueue, Instant)>> = (0..5).map(|_| None).collect();
        let mut remaining = [1800_usize; 5];
        let mut accepted = 0;
        let mut refused = 0;
        let mut max_wait = Duration::ZERO;
        let mut pause = pause;
        let mut paused = false;
        let mut released_before_pause = false;
        let mut steps = 0;
        let watchdog = Instant::now();
        loop {
            steps += 1;
            assert!(steps < 50_000 && watchdog.elapsed() < Duration::from_secs(30));
            for (producer, left) in producers.iter_mut().zip(&mut remaining) {
                loop {
                    if producer.is_none() && *left > 0 {
                        let call = clock.now();
                        // Max spec, bounded record, bounded cleanup.log.
                        let charge = match *left % 3 {
                            0 => 264 * 1024,
                            _ => 72 * 1024,
                        };
                        let pool = pool.clone();
                        let cancel = cancel.clone();
                        *producer = Some((
                            Box::pin(async move { pool.acquire(charge, call, &cancel).await }),
                            call,
                        ));
                    }
                    let Some((future, call)) = producer else {
                        break;
                    };
                    match poll(future.as_mut()) {
                        Poll::Pending => break,
                        Poll::Ready(result) => {
                            let waited = clock.now().duration_since(*call);
                            max_wait = max_wait.max(waited);
                            match result {
                                Ok(credit) => {
                                    writing.push_back(Accepted {
                                        credit,
                                        publication: pool
                                            .publication(*call + Duration::from_secs(5)),
                                        call: *call,
                                    });
                                    accepted += 1;
                                }
                                Err(error) => {
                                    assert_eq!(error, PersistError::QueueFull);
                                    refused += 1;
                                }
                            }
                            *producer = None;
                            *left -= 1;
                        }
                    }
                }
            }
            if writing.is_empty() {
                break;
            }
            if let Some((between, delay)) = pause {
                if !pool.lock().waiters.is_empty() {
                    if between && !released_before_pause {
                        let mut write = writing.pop_front().unwrap();
                        clock.advance(Duration::from_millis(cycle_ms));
                        assert!(!write.publication.returned());
                        drop(write.credit);
                        // This return may grant every current wait. Let the
                        // producers (which take no clock time) take those
                        // grants and queue again until one waits; the pause
                        // then follows this release with no write between.
                        released_before_pause = true;
                        continue;
                    }
                    // Tick the wait limit before completing the 1.2 s pause.
                    let call = pool.lock().waiters.front().unwrap().call;
                    let until = call + WAIT_LIMIT;
                    let before = clock.now();
                    clock.advance(until.saturating_duration_since(before));
                    pool.lock().update(clock.now(), false);
                    clock.advance((before + delay).saturating_duration_since(clock.now()));
                    pause = None;
                    paused = true;
                    continue;
                }
            }
            let mut write = writing.pop_front().unwrap();
            // Budget expiration can precede the next scripted write return.
            let budget_left = {
                let state = pool.lock();
                state.budget.saturating_sub(state.waited)
            };
            let cycle = Duration::from_millis(cycle_ms);
            if !pool.lock().waiters.is_empty() && budget_left < cycle {
                clock.advance(budget_left);
                pool.lock().update(clock.now(), false);
                clock.advance(cycle - budget_left);
            } else {
                clock.advance(cycle);
            }
            assert!(clock.now() < write.call + Duration::from_secs(5));
            assert!(!write.publication.returned(), "accepted artefact timed out");
            drop(write.credit);
        }
        assert_eq!(accepted + refused, 9000);
        assert_eq!(pool.lock().used, 0);
        assert!(pool.lock().peak <= CAPACITY);
        assert!(pool.lock().waited <= Duration::from_secs(46));
        assert!(!pool.unavailable.load(Ordering::Acquire));
        if paused {
            assert!(refused > 0);
            // The driver observes the 1.2 s pause after its exact 1 s refusal;
            // dedicated boundary tests above check the refusal instant itself.
        } else if cycle_ms == 4 {
            assert_eq!(refused, 0);
            assert!(max_wait < WAIT_LIMIT);
        } else {
            assert!(refused > 0);
            assert!(max_wait < WAIT_LIMIT);
            assert_eq!(pool.lock().waited, Duration::from_secs(46));
        }
        // W drained all ordinary publications. S takes its reserved lane,
        // independent of nonblocking mode, cancellation, and all queue credits.
        cancel.cancel();
        let mut report = pool.publication(clock.now() + Duration::from_secs(5));
        clock.advance(Duration::from_millis(4));
        assert!(!report.returned());
        assert_eq!(pool.lock().used, 0);
    }

    #[test]
    fn three_thousand_executions_five_producers_four_ms_cycles() {
        script(4, None);
    }
    #[test]
    fn three_thousand_executions_six_ms_cycles_exhaust_union_budget() {
        script(6, None);
    }
    #[test]
    fn three_thousand_executions_with_one_1200ms_write() {
        script(4, Some((false, Duration::from_millis(1200))));
    }
    #[test]
    fn three_thousand_executions_with_one_1200ms_scheduling_pause() {
        script(4, Some((true, Duration::from_millis(1200))));
    }
}
