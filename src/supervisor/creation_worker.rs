//! Slot-free ownership of a job's creation thread.
use std::io;
use std::thread::{self, JoinHandle};

#[derive(Debug)]
pub(super) struct CreationWorker<C> {
    thread: Option<JoinHandle<()>>,
    credit: Option<C>,
}

impl<C: Send + 'static> CreationWorker<C> {
    pub(super) fn spawn(credit: C, work: impl FnOnce() + Send + 'static) -> io::Result<Self> {
        Self::spawn_with_tail(
            credit,
            work,
            #[cfg(test)]
            || {},
        )
    }

    // Tests pause here to expose the interval between finishing work and
    // exiting the OS thread. The tail is absent from non-test builds.
    fn spawn_with_tail(
        credit: C,
        work: impl FnOnce() + Send + 'static,
        #[cfg(test)] tail: impl FnOnce() + Send + 'static,
    ) -> io::Result<Self> {
        let thread = thread::Builder::new()
            .name("cpf-create".into())
            .spawn(move || {
                work();
                #[cfg(test)]
                tail();
            })?;
        Ok(Self {
            thread: Some(thread),
            credit: Some(credit),
        })
    }

    pub(super) fn resources_released(&mut self) -> bool {
        if self.thread.as_ref().is_some_and(JoinHandle::is_finished) {
            let _ = self.thread.take().expect("finished creation worker").join();
            self.credit.take();
        }
        self.thread.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, mpsc};
    use std::time::{Duration, Instant};

    struct Credit(Arc<AtomicUsize>);
    impl Drop for Credit {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::AcqRel);
        }
    }

    #[test]
    fn creation_credit_is_held_until_the_thread_is_joined() {
        let used = Arc::new(AtomicUsize::new(1));
        let (entered, waiting) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let mut worker = CreationWorker::spawn_with_tail(
            Credit(used.clone()),
            || {},
            move || {
                entered.send(()).unwrap();
                released.recv_timeout(Duration::from_secs(5)).unwrap();
            },
        )
        .unwrap();
        waiting.recv_timeout(Duration::from_secs(5)).unwrap();
        let released_while_alive = worker.resources_released();
        let held_while_alive = used.load(Ordering::Acquire);
        // Release before asserting so a failing test cannot strand a thread.
        release.send(()).unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        while !worker.resources_released() {
            assert!(Instant::now() < until, "creation worker did not exit");
            thread::yield_now();
        }
        assert!(!released_while_alive, "live creation thread was released");
        assert_eq!(
            held_while_alive, 1,
            "live creation thread returned its credit"
        );
        assert_eq!(used.load(Ordering::Acquire), 0);
    }

    #[test]
    fn completed_creation_worker_returns_its_credit() {
        let used = Arc::new(AtomicUsize::new(1));
        let mut worker = CreationWorker::spawn(Credit(used.clone()), || {}).unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        while !worker.resources_released() {
            assert!(Instant::now() < until, "creation worker did not exit");
            thread::yield_now();
        }
        assert_eq!(used.load(Ordering::Acquire), 0);
    }
}
