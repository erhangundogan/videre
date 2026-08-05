use std::sync::{Condvar, Mutex};

/// A tiny counting semaphore for bounding how many callers run a section of
/// code concurrently. Used to cap concurrent `qlmanage` subprocess launches:
/// QuickLook's thumbnail-generation agent is a shared per-user service that
/// doesn't parallelize well, so letting many callers (e.g. a UI rendering
/// hundreds of HEIC thumbnails at once) spawn `qlmanage` unbounded causes the
/// agent, and the source drive's I/O, to queue up and occasionally exceed
/// even a generous per-call timeout.
pub struct Semaphore {
    state: Mutex<usize>,
    cond: Condvar,
    max: usize,
}

pub struct SemaphorePermit<'a> {
    sem: &'a Semaphore,
}

impl Drop for SemaphorePermit<'_> {
    fn drop(&mut self) {
        let mut count = self.sem.state.lock().unwrap();
        *count -= 1;
        self.sem.cond.notify_one();
    }
}

impl Semaphore {
    pub fn new(max: usize) -> Self {
        Semaphore { state: Mutex::new(0), cond: Condvar::new(), max }
    }

    /// Blocks until fewer than `max` permits are held, then takes one.
    /// Released automatically when the returned guard drops.
    pub fn acquire(&self) -> SemaphorePermit<'_> {
        let mut count = self.state.lock().unwrap();
        while *count >= self.max {
            count = self.cond.wait(count).unwrap();
        }
        *count += 1;
        SemaphorePermit { sem: self }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn never_exceeds_max_concurrent_holders() {
        let sem = Arc::new(Semaphore::new(2));
        let current = Arc::new(AtomicUsize::new(0));
        let max_observed = Arc::new(AtomicUsize::new(0));

        let handles: Vec<_> = (0..8)
            .map(|_| {
                let sem = Arc::clone(&sem);
                let current = Arc::clone(&current);
                let max_observed = Arc::clone(&max_observed);
                thread::spawn(move || {
                    let _permit = sem.acquire();
                    let now = current.fetch_add(1, Ordering::SeqCst) + 1;
                    max_observed.fetch_max(now, Ordering::SeqCst);
                    thread::sleep(Duration::from_millis(20));
                    current.fetch_sub(1, Ordering::SeqCst);
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        assert!(max_observed.load(Ordering::SeqCst) <= 2);
    }
}
