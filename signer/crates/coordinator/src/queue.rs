//! H-12: the proving queue.
//!
//! A deposit proof takes about 10 GB of RAM on a CPU, or 13 GB of GPU memory, for minutes to
//! hours (docs/TESTING.md). Proofs used to start as soon as each vault's deposit was confirmed, all
//! at once, so a few cheap deposits could exhaust the machine that also serves the API. Jobs now
//! wait in one queue and run `concurrency` at a time; a key (a vault, or a payout) has at most one
//! job, queued or running, and asking again for its proof joins that job.
//!
//! v4 (section 3) proves every payout too, so jobs have a [`Priority`]: deposits first (a user
//! waits for the mint), then disputed payouts (a deadline), then swap settlements (track SWAP's
//! maker, through [`ProvingQueue::enqueue`]), then routine inclusion proofs; first in first out
//! within a priority. Asking again at a higher priority moves a waiting job up; nothing ever
//! interrupts a running proof.

use std::{
    collections::HashSet,
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

/// What a proof is for, most urgent first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    /// A deposit, as collateral or as a refund: a user waits for it.
    Deposit,
    /// The inclusion of a payout whose confirmation is disputed, or a reopened claim's (T-05 (a),
    /// D57): due within the Registry's inclusion window.
    Dispute,
    /// A swap's settlement (section 5), proven by the maker: due before the order's deadline.
    SwapSettlement,
    /// The routine proof of a payout's delivery (section 3): it makes the claims final.
    RoutineInclusion,
}

/// What became of a request for a proof.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Enqueued {
    /// A new job, in its priority's line (or already started if a slot was free).
    Queued,
    /// The key already has a job, queued or running: this request joins it (moving a waiting job
    /// up if this request is more urgent).
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

struct Waiting {
    key: B256,
    priority: Priority,
    /// Order of arrival within a priority.
    seq: u64,
    job: Job,
}

#[derive(Default)]
struct State {
    waiting: Vec<Waiting>,
    running: HashSet<B256>,
    next_seq: u64,
}

impl State {
    /// The waiting job that runs next: the most urgent, the oldest of those.
    fn next(&mut self) -> Option<Waiting> {
        let i = self.waiting.iter().enumerate().min_by_key(|(_, w)| (w.priority, w.seq)).map(|(i, _)| i)?;
        Some(self.waiting.remove(i))
    }
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

    /// Queues `job` for `key` at `priority`, unless the key already has a job queued or running
    /// (a waiting one moves up to `priority` if that is more urgent). Jobs start as slots free up,
    /// the most urgent first, in arrival order within a priority. Needs a Tokio runtime.
    pub fn enqueue(
        self: &Arc<Self>,
        key: B256,
        priority: Priority,
        job: impl Future<Output = ()> + Send + 'static,
    ) -> Enqueued {
        {
            let mut s = self.lock();
            if s.running.contains(&key) {
                return Enqueued::Joined;
            }
            if let Some(w) = s.waiting.iter_mut().find(|w| w.key == key) {
                if priority < w.priority {
                    w.priority = priority;
                }
                return Enqueued::Joined;
            }
            let seq = s.next_seq;
            s.next_seq += 1;
            s.waiting.push(Waiting {
                key,
                priority,
                seq,
                job: Box::pin(job),
            });
        }
        self.dispatch();
        Enqueued::Queued
    }

    /// Whether `key` has a job, queued or running.
    pub fn contains(&self, key: &B256) -> bool {
        let s = self.lock();
        s.running.contains(key) || s.waiting.iter().any(|w| w.key == *key)
    }

    pub fn counts(&self) -> QueueCounts {
        let s = self.lock();
        QueueCounts {
            queued: s.waiting.len(),
            running: s.running.len(),
            concurrency: self.concurrency,
        }
    }

    /// Starts the most urgent waiting jobs while slots are free. A finished job, failed or
    /// panicked, frees its slot and starts the next one.
    fn dispatch(self: &Arc<Self>) {
        let mut s = self.lock();
        while s.running.len() < self.concurrency {
            let Some(next) = s.next() else {
                break;
            };
            s.running.insert(next.key);
            let queue = Arc::clone(self);
            let (key, job) = (next.key, next.job);
            tokio::spawn(async move {
                // A panic inside a job is the job's failure; the queue goes on.
                let _ = AssertUnwindSafe(job).catch_unwind().await;
                queue.lock().running.remove(&key);
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

    fn key(i: u8) -> B256 {
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

    /// Jobs of one priority start in the order they were queued, one at a time by default.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn jobs_run_first_in_first_out() {
        let q = ProvingQueue::new(1).unwrap();
        let (mut c, mut started) = Controlled::new();
        for i in 1..=4 {
            assert_eq!(q.enqueue(key(i), Priority::Deposit, c.job(i)), Enqueued::Queued);
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

    /// Section 3: deposits first, then disputed payouts, then swap settlements, then routine
    /// inclusion proofs, whatever order they were asked in; a running proof is never interrupted.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_most_urgent_job_goes_first() {
        let q = ProvingQueue::new(1).unwrap();
        let (mut c, mut started) = Controlled::new();
        // Key 1 runs; then, in this order: a routine proof, a swap, a dispute, a deposit, another
        // routine proof.
        assert_eq!(q.enqueue(key(1), Priority::RoutineInclusion, c.job(1)), Enqueued::Queued);
        assert_eq!(next_start(&mut started).await, 1);
        let asked = [
            (2, Priority::RoutineInclusion),
            (3, Priority::SwapSettlement),
            (4, Priority::Dispute),
            (5, Priority::Deposit),
            (6, Priority::RoutineInclusion),
        ];
        for (i, p) in asked {
            assert_eq!(q.enqueue(key(i), p, c.job(i)), Enqueued::Queued);
        }
        no_start(&mut started).await;
        let mut order = vec![];
        let mut running = 1;
        for _ in 0..asked.len() {
            c.finish(running);
            running = next_start(&mut started).await;
            order.push(running);
        }
        assert_eq!(order, vec![5, 4, 3, 2, 6]);
        c.finish(running);
        settle(&q, 0, 0).await;
    }

    /// A routine proof that becomes a dispute's (the same payout) moves up; a less urgent request
    /// never moves a job down.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_waiting_job_moves_up_to_a_more_urgent_request() {
        let q = ProvingQueue::new(1).unwrap();
        let (mut c, mut started) = Controlled::new();
        q.enqueue(key(1), Priority::Deposit, c.job(1));
        assert_eq!(next_start(&mut started).await, 1);
        q.enqueue(key(2), Priority::SwapSettlement, c.job(2));
        q.enqueue(key(3), Priority::RoutineInclusion, c.job(3));
        assert_eq!(q.enqueue(key(3), Priority::Dispute, async {}), Enqueued::Joined);
        assert_eq!(q.enqueue(key(2), Priority::RoutineInclusion, async {}), Enqueued::Joined);
        c.finish(1);
        assert_eq!(next_start(&mut started).await, 3, "moved up to a dispute");
        c.finish(3);
        assert_eq!(next_start(&mut started).await, 2, "never moved down");
        c.finish(2);
        settle(&q, 0, 0).await;
    }

    /// A key has one job at most: asking again while it waits or runs joins it; once it is over
    /// (a failed proof), it can be queued again, behind the jobs of its priority.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn one_job_per_key() {
        let q = ProvingQueue::new(1).unwrap();
        let (mut c, mut started) = Controlled::new();
        assert_eq!(q.enqueue(key(1), Priority::Deposit, c.job(1)), Enqueued::Queued);
        assert_eq!(next_start(&mut started).await, 1);
        assert_eq!(q.enqueue(key(2), Priority::Deposit, c.job(2)), Enqueued::Queued);
        let ran = Arc::new(AtomicUsize::new(0));
        for v in [1, 2] {
            let ran = ran.clone();
            let duplicate = async move {
                ran.fetch_add(1, Ordering::SeqCst);
            };
            assert_eq!(q.enqueue(key(v), Priority::Deposit, duplicate), Enqueued::Joined, "key {v}");
        }
        assert!(q.contains(&key(1)) && q.contains(&key(2)) && !q.contains(&key(3)));
        settle(&q, 1, 1).await;
        c.finish(1);
        assert_eq!(next_start(&mut started).await, 2);
        settle(&q, 0, 1).await;
        assert!(!q.contains(&key(1)));
        // Key 1's proof failed: the next pass queues it again, behind key 3.
        assert_eq!(q.enqueue(key(3), Priority::Deposit, c.job(3)), Enqueued::Queued);
        assert_eq!(q.enqueue(key(1), Priority::Deposit, c.job(1)), Enqueued::Queued);
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
            let priority = if i % 2 == 0 {
                Priority::Deposit
            } else {
                Priority::RoutineInclusion
            };
            q.enqueue(key(i), priority, async move {
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
        q.enqueue(key(1), Priority::Deposit, async { panic!("the job panics (test)") });
        q.enqueue(key(2), Priority::Deposit, c.job(2));
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
