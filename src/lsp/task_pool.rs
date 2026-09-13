//! A minimal fixed-size worker thread pool, modeled on rust-analyzer's
//! `TaskPool`.
//!
//! The LSP keeps latency-sensitive reads (formatting, the parse-diagnostics
//! read-phase) on a dedicated [`TaskPool`] sized to the machine's parallelism,
//! off the single-writer worker thread.
//!
//! Jobs are fire-and-forget closures that post their own results through
//! whatever channels they capture (the LSP `out_tx`/`done_tx`), so the pool
//! needs no result channel of its own — just [`Spawner::spawn`].

use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender};

/// A boxed unit of work to run on a worker thread.
type Job = Box<dyn FnOnce() + Send + 'static>;

/// A fixed pool of worker threads consuming boxed closures.
///
/// Shutdown drains assigned jobs and joins workers, releasing their captured
/// state before the host returns from the session.
pub(crate) struct TaskPool {
    job_tx: Sender<Option<Job>>,
    permits: Receiver<()>,
    permit_tx: Sender<()>,
    _workers: Vec<JoinHandle<()>>,
}

impl TaskPool {
    /// Spawn `n` worker threads (clamped to at least 1), each named `name`.
    pub(crate) fn new(name: &'static str, n: usize) -> Self {
        let n = n.max(1);
        let (job_tx, job_rx) = crossbeam_channel::unbounded::<Option<Job>>();
        let (permit_tx, permits) = crossbeam_channel::bounded(n);
        for _ in 0..n {
            permit_tx.send(()).expect("new pool");
        }
        let workers = (0..n)
            .map(|_| {
                let job_rx = job_rx.clone();
                std::thread::Builder::new()
                    .name(name.to_owned())
                    .spawn(move || {
                        // Exits cleanly when all `job_tx` clones drop.
                        for job in job_rx {
                            let Some(job) = job else { break };
                            // Catch genuine panics so one buggy job can't
                            // permanently take a worker out of rotation — rayon
                            // isolated panics per task, and raw threads don't.
                            // Salsa `Cancelled` never reaches here: the read
                            // helpers and the analyze site catch it upstream.
                            if let Err(panic) =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(job))
                            {
                                let msg = panic
                                    .downcast_ref::<&'static str>()
                                    .copied()
                                    .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
                                    .unwrap_or("<non-string panic payload>");
                                log::error!("LSP task pool worker caught panic: {msg}");
                            }
                        }
                    })
                    .expect("failed to spawn LSP worker thread")
            })
            .collect();
        Self {
            job_tx,
            permits,
            permit_tx,
            _workers: workers,
        }
    }

    /// A cheap, cloneable handle for submitting work to this pool.
    pub(crate) fn spawner(&self) -> Spawner {
        Spawner {
            jobs: self.job_tx.clone(),
            permits: self.permits.clone(),
            permit_tx: self.permit_tx.clone(),
        }
    }
}

impl Drop for TaskPool {
    fn drop(&mut self) {
        for _ in &self._workers {
            let _ = self.job_tx.send(None);
        }
        for worker in self._workers.drain(..) {
            let _ = worker.join();
        }
    }
}

/// A cloneable submit-side handle onto a [`TaskPool`], shareable across the main
/// loop and the worker thread.
#[derive(Clone)]
pub(crate) struct Spawner {
    jobs: Sender<Option<Job>>,
    permits: Receiver<()>,
    permit_tx: Sender<()>,
}

/// Capacity is reserved before capturing an analysis snapshot. Only jobs with
/// an assigned execution slot can own a snapshot, including during dispatch.
pub(crate) struct ReadPermit(Sender<()>);
impl Drop for ReadPermit {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

impl Spawner {
    pub(crate) fn available(&self) -> bool {
        !self.permits.is_empty()
    }
    pub(crate) fn try_reserve(&self) -> Option<ReadPermit> {
        self.permits.try_recv().ok()?;
        Some(ReadPermit(self.permit_tx.clone()))
    }
    pub(crate) fn reserve(&self) -> ReadPermit {
        self.permits.recv().expect("live read pool");
        ReadPermit(self.permit_tx.clone())
    }
    pub(crate) fn spawn_reserved(&self, permit: ReadPermit, f: impl FnOnce() + Send + 'static) {
        let _ = self.jobs.send(Some(Box::new(move || {
            let _permit = permit;
            f();
        })));
    }
    /// Hand a closure to the pool. It runs on some worker thread. Sending only
    /// fails once every worker has died, which we treat as shutdown.
    #[cfg(test)]
    pub(crate) fn spawn(&self, f: impl FnOnce() + Send + 'static) {
        self.spawn_reserved(self.reserve(), f);
    }
}

/// Worker count for the read pool: the machine's available parallelism.
pub(crate) fn read_pool_size() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn every_spawned_job_runs() {
        let pool = TaskPool::new("test-pool", 4);
        let spawner = pool.spawner();
        let (tx, rx) = crossbeam_channel::unbounded::<usize>();
        const N: usize = 64;
        for i in 0..N {
            let tx = tx.clone();
            spawner.spawn(move || {
                let _ = tx.send(i);
            });
        }
        drop(tx);
        let mut seen: Vec<usize> = rx.iter().collect();
        seen.sort_unstable();
        assert_eq!(seen, (0..N).collect::<Vec<_>>());
    }

    #[test]
    fn panicking_job_does_not_kill_the_pool() {
        // A single worker runs jobs in submission order: the panic lands first,
        // then the survivor must still run on the same (only) worker. If the
        // panic took the worker out of rotation, the survivor never runs.
        let pool = TaskPool::new("test-pool-panic", 1);
        let spawner = pool.spawner();
        let ran = Arc::new(AtomicUsize::new(0));

        spawner.spawn(|| panic!("boom"));

        let ran2 = Arc::clone(&ran);
        let (done_tx, done_rx) = crossbeam_channel::bounded::<()>(1);
        spawner.spawn(move || {
            ran2.fetch_add(1, Ordering::SeqCst);
            let _ = done_tx.send(());
        });

        done_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("survivor job should run after a panicking job");
        assert_eq!(ran.load(Ordering::SeqCst), 1);
    }
}

#[cfg(test)]
mod reservation_tests {
    use super::*;
    #[test]
    fn capacity_is_held_until_captured_values_drop_even_after_a_panic() {
        let pool = TaskPool::new("reservation-test", 1);
        let spawner = pool.spawner();
        let permit = spawner.reserve();
        assert!(spawner.permits.try_recv().is_err());
        let (entered_tx, entered_rx) = crossbeam_channel::bounded(1);
        let (release_tx, release_rx) = crossbeam_channel::bounded(1);
        let retained = std::sync::Arc::new(());
        let weak = std::sync::Arc::downgrade(&retained);
        spawner.spawn_reserved(permit, move || {
            let _retained = retained;
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            panic!("isolated query panic");
        });
        entered_rx.recv().unwrap();
        assert!(spawner.permits.try_recv().is_err());
        release_tx.send(()).unwrap();
        let _next = spawner.reserve();
        assert!(weak.upgrade().is_none());
    }
}
