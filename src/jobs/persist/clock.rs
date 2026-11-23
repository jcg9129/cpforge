//! One monotonic clock for producer waits, publication and the worker's decision.
use std::time::Instant;

#[derive(Clone, Debug, Default)]
pub(crate) struct Clock {
    #[cfg(test)]
    manual: Option<std::sync::Arc<Manual>>,
}
impl Clock {
    pub(crate) fn now(&self) -> Instant {
        #[cfg(test)]
        if let Some(clock) = &self.manual {
            return *clock.now.lock().unwrap();
        }
        Instant::now()
    }

    pub(crate) async fn until(&self, deadline: Instant) {
        #[cfg(test)]
        if let Some(clock) = &self.manual {
            loop {
                let changed = clock.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if self.now() >= deadline {
                    return;
                }
                changed.await;
            }
        }
        tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
    }

    #[cfg(test)]
    pub(crate) fn controlled() -> Self {
        Self {
            manual: Some(std::sync::Arc::new(Manual {
                now: std::sync::Mutex::new(Instant::now()),
                changed: tokio::sync::Notify::new(),
            })),
        }
    }

    #[cfg(test)]
    pub(crate) fn advance(&self, elapsed: std::time::Duration) {
        let clock = self.manual.as_ref().expect("controlled clock");
        *clock.now.lock().unwrap() += elapsed;
        clock.changed.notify_waiters();
    }
}

#[cfg(test)]
#[derive(Debug)]
struct Manual {
    now: std::sync::Mutex<Instant>,
    changed: tokio::sync::Notify,
}
