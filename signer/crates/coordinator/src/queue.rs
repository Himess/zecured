//! H-12: the deposit-proof queue.
//!
//! A deposit proof takes about 10 GB of RAM on a CPU, or 13 GB of GPU memory, for minutes to
//! hours (docs/TESTING.md). Proofs used to start as soon as each vault's deposit was confirmed, all
//! at once, so a few cheap deposits could exhaust the machine that also serves the API. Jobs now
//! wait in one queue, first in first out, and run `concurrency` at a time; a vault has at most one
//! job, queued or running, and asking again for its proof joins that job.

use std::{
    collections::{HashSet, VecDeque},
    future::Future,
    panic::AssertUnwindSafe,
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard},
};

use alloy::primitives::B256;
use futures::FutureExt;

/// Most proofs that may run at once.
pub const MAX_CONCURRENCY: usize = 2;

type Job = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// What became of a request for a vault's proof.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Enqueued {
    /// A new job, at the end of the queue (or already started if a slot was free).
    Queued,
    /// The vault already has a job, queued or running: this request joins it.
    Joined,
}

/// Jobs waiting and running, counts only: nothing about which vaults they are for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueCounts {
    pub queued: usize,
    pub running: usize,
    pub concurrency: usize,
}

#[derive(Default)]
struct State {
    waiting: VecDeque<(B256, Job)>,
    running: HashSet<B256>,
}

pub struct ProvingQueue {
    concurrency: usize,
    state: Mutex<State>,
}

impl ProvingQueue {
    /// A queue running `concurrency` jobs at a time (1 to [`MAX_CONCURRENCY`]).
    pub fn new(concurrency: usize) -> anyhow::Result<Arc<Self>> {
        anyhow::ensure!(
            (1..=MAX_CONCURRENCY).contains(&concurrency),
            "deposit proofs run 1 to {MAX_CONCURRENCY} at a time (asked: {concurrency})"
        );
        Ok(Arc::new(Self {
            concurrency,
            state: Mutex::default(),
        }))
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Queues `job` for `vault`, unless the vault already has a job queued or running. Jobs start
    /// in the order they were queued, as slots free up. Needs a Tokio runtime.
    pub fn enqueue(self: &Arc<Self>, vault: B256, job: impl Future<Output = ()> + Send + 'static) -> Enqueued {
        {
            let mut s = self.lock();
            if s.running.contains(&vault) || s.waiting.iter().any(|(v, _)| *v == vault) {
                return Enqueued::Joined;
            }
            s.waiting.push_back((vault, Box::pin(job)));
        }
        self.dispatch();
        Enqueued::Queued
    }

    /// Whether `vault` has a job, queued or running.
    pub fn contains(&self, vault: &B256) -> bool {
        let s = self.lock();
        s.running.contains(vault) || s.waiting.iter().any(|(v, _)| v == vault)
    }

    pub fn counts(&self) -> QueueCounts {
        let s = self.lock();
        QueueCounts {
            queued: s.waiting.len(),
            running: s.running.len(),
            concurrency: self.concurrency,
        }
    }

    /// Starts the oldest waiting jobs while slots are free. A finished job, failed or panicked,
    /// frees its slot and starts the next one.
    fn dispatch(self: &Arc<Self>) {
        let mut s = self.lock();
        while s.running.len() < self.concurrency {
            let Some((vault, job)) = s.waiting.pop_front() else {
                break;
            };
            s.running.insert(vault);
            let queue = Arc::clone(self);
            tokio::spawn(async move {
                // A panic inside a job is the job's failure; the queue goes on.
                let _ = AssertUnwindSafe(job).catch_unwind().await;
                queue.lock().running.remove(&vault);
                queue.dispatch();
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };

    use tokio::sync::{mpsc, oneshot};

    use super::*;

    fn vault(i: u8) -> B256 {
        B256::repeat_byte(i)
    }

    /// A job that reports when it starts and finishes when the test says so.
    struct Controlled {
        started: mpsc::UnboundedSender<u8>,
        finish: HashMap<u8, oneshot::Sender<()>>,
    }

    impl Controlled {
        fn new() -> (Self, mpsc::UnboundedReceiver<u8>) {
            let (started, rx) = mpsc::unbounded_channel();
            (
                Self {
                    started,
                    finish: HashMap::new(),
                },
                rx,
            )
        }

        fn job(&mut self, i: u8) -> impl Future<Output = ()> + Send + 'static {
            let (tx, rx) = oneshot::channel();
            self.finish.insert(i, tx);
            let started = self.started.clone();
            async move {
                let _ = started.send(i);
                let _ = rx.await;
            }
        }

        fn finish(&mut self, i: u8) {
            let _ = self.finish.remove(&i).unwrap().send(());
        }
    }

    async fn next_start(rx: &mut mpsc::UnboundedReceiver<u8>) -> u8 {
        tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.expect("a job starts").unwrap()
    }

    async fn no_start(rx: &mut mpsc::UnboundedReceiver<u8>) {
        assert!(tokio::time::timeout(Duration::from_millis(100), rx.recv()).await.is_err(), "no job may start");
    }

    async fn settle(q: &ProvingQueue, queued: usize, running: usize) {
        for _ in 0..500 {
            let c = q.counts();
            if (c.queued, c.running) == (queued, running) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("expected {queued} queued and {running} running, got {:?}", q.counts());
    }

    /// Jobs start in the order they were queued, one at a time by default.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn jobs_run_first_in_first_out() {
        let q = ProvingQueue::new(1).unwrap();
        let (mut c, mut started) = Controlled::new();
        for i in 1..=4 {
            assert_eq!(q.enqueue(vault(i), c.job(i)), Enqueued::Queued);
        }
        assert_eq!(next_start(&mut started).await, 1);
        no_start(&mut started).await;
        settle(&q, 3, 1).await;
        for i in 1..=3 {
            c.finish(i);
            assert_eq!(next_start(&mut started).await, i + 1);
            no_start(&mut started).await;
        }
        c.finish(4);
        settle(&q, 0, 0).await;
    }

    /// A vault has one job at most: asking again while it waits or runs joins it; once it is over
    /// (a failed proof), it can be queued again, at the end of the queue.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn one_job_per_vault() {
        let q = ProvingQueue::new(1).unwrap();
        let (mut c, mut started) = Controlled::new();
        assert_eq!(q.enqueue(vault(1), c.job(1)), Enqueued::Queued);
        assert_eq!(next_start(&mut started).await, 1);
        assert_eq!(q.enqueue(vault(2), c.job(2)), Enqueued::Queued);
        let ran = Arc::new(AtomicUsize::new(0));
        for v in [1, 2] {
            let ran = ran.clone();
            let duplicate = async move {
                ran.fetch_add(1, Ordering::SeqCst);
            };
            assert_eq!(q.enqueue(vault(v), duplicate), Enqueued::Joined, "vault {v}: running, then queued");
        }
        assert!(q.contains(&vault(1)) && q.contains(&vault(2)) && !q.contains(&vault(3)));
        settle(&q, 1, 1).await;
        c.finish(1);
        assert_eq!(next_start(&mut started).await, 2);
        settle(&q, 0, 1).await;
        assert!(!q.contains(&vault(1)));
        // Vault 1's proof failed: the next pass queues it again, behind vault 3.
        assert_eq!(q.enqueue(vault(3), c.job(3)), Enqueued::Queued);
        assert_eq!(q.enqueue(vault(1), c.job(1)), Enqueued::Queued);
        c.finish(2);
        assert_eq!(next_start(&mut started).await, 3);
        c.finish(3);
        assert_eq!(next_start(&mut started).await, 1);
        c.finish(1);
        settle(&q, 0, 0).await;
        assert_eq!(ran.load(Ordering::SeqCst), 0, "a joined request never runs a second job");
    }

    /// Never more jobs at once than the concurrency, and every job runs.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_concurrency_is_bounded() {
        let q = ProvingQueue::new(MAX_CONCURRENCY).unwrap();
        let (now, max, done) =
            (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        for i in 1..=12 {
            let (now, max, done) = (now.clone(), max.clone(), done.clone());
            q.enqueue(vault(i), async move {
                let running = now.fetch_add(1, Ordering::SeqCst) + 1;
                max.fetch_max(running, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(20)).await;
                now.fetch_sub(1, Ordering::SeqCst);
                done.fetch_add(1, Ordering::SeqCst);
            });
            assert!(q.counts().running <= MAX_CONCURRENCY);
        }
        settle(&q, 0, 0).await;
        assert_eq!(done.load(Ordering::SeqCst), 12);
        assert_eq!(max.load(Ordering::SeqCst), MAX_CONCURRENCY);
    }

    /// A panicking job frees its slot; the next one starts.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_panicking_job_frees_its_slot() {
        let q = ProvingQueue::new(1).unwrap();
        let (mut c, mut started) = Controlled::new();
        q.enqueue(vault(1), async { panic!("the job panics (test)") });
        q.enqueue(vault(2), c.job(2));
        assert_eq!(next_start(&mut started).await, 2);
        c.finish(2);
        settle(&q, 0, 0).await;
    }

    #[test]
    fn the_concurrency_is_one_or_two() {
        assert!(ProvingQueue::new(0).is_err());
        assert!(ProvingQueue::new(3).is_err());
        assert_eq!(
            ProvingQueue::new(2).unwrap().counts(),
            QueueCounts {
                queued: 0,
                running: 0,
                concurrency: 2
            }
        );
    }
}
