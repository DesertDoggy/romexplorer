//! Many files at once: the one skeleton every multi-file job runs on.
//!
//! A scan, a copy, a conversion and chdlite's batch are the same loop with different
//! arguments: list the inputs, run one [`Single`] per input -- several at a time, limited
//! per size tier -- and hand each result back the moment it finishes, so the caller can
//! record it (a database row, a log line) while the rest keep going.
//!
//! Files fall into three size tiers, each with its own limit, because what is "a lot at once"
//! differs by orders of magnitude between them: sixteen 200 KB cartridge dumps are a few
//! milliseconds of reads, sixteen 8 GB discs are a disk saturated for an hour.
//!
//! | tier | size | why the boundary |
//! |---|---|---|
//! | small | up to 16 MiB | `hash::FileSizeClass::SMALL_MAX_BYTES`: a file read in a handful of buffers |
//! | medium | 16 MiB - 1 GiB | up to `operation::ContainerSizeClass::SMALL_MAX_BYTES`: CD-sized |
//! | large | 1 GiB and up | DVD / Blu-ray-sized discs and containers |
//!
//! **Auto** ([`auto_limits`]) asks [`recommended_parallel_files`] -- storage kind, RAM,
//! physical cores and the hash set -- for the small tier, and its large-file answer for
//! medium. Large takes that answer but at most 2: past that, two big sequential reads already
//! keep any single disk busy, and a third only makes all three seek.
//!
//! The limits are per tier, not shared: with [`Multiple::run`] a copy can be writing 16 small
//! files while 2 large ones run -- a big disc should not hold up the dumps queued behind it.
//! A scan uses [`Multiple::run_in_turn`] instead: one first-level folder at a time, and in
//! it one size at a time, smallest first, so a hard disk reads one kind of file at a time
//! and the number of files at once is never more than the running size's own limit.
//!
//! The work itself is blocking code (reads, decodes, hashes, the native archivers) and runs
//! on tokio's blocking pool, one thread per running file; the gate and the result stream are
//! async, so a caller on a tokio runtime (the app's jobs, chdlite's GUI and CLI) awaits
//! results without parking a thread per file.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use futures_util::stream::FuturesUnordered;
use serde::Serialize;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::hash::{FileSizeClass, HashFlags, RamClass, StorageKind};
use super::operation::{recommended_parallel_files, ContainerSizeClass, JobStages, ProgressEvent, Single, SingleArgs, SingleError, SingleOutcome};

/// Up to and including this many bytes is the small tier.
pub const SMALL_MAX_BYTES: u64 = FileSizeClass::SMALL_MAX_BYTES;
/// Below this many bytes (and above small) is the medium tier.
pub const MEDIUM_MAX_BYTES: u64 = ContainerSizeClass::SMALL_MAX_BYTES;
/// Auto's ceiling for the large tier.
const AUTO_LARGE_MAX: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tier {
    Small,
    Medium,
    Large,
}

impl Tier {
    pub fn of(len: u64) -> Self {
        if len <= SMALL_MAX_BYTES {
            Self::Small
        } else if len < MEDIUM_MAX_BYTES {
            Self::Medium
        } else {
            Self::Large
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Small => 0,
            Self::Medium => 1,
            Self::Large => 2,
        }
    }
}

/// Files at once, per tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Limits {
    pub small: usize,
    pub medium: usize,
    pub large: usize,
}

impl Limits {
    pub fn get(&self, tier: Tier) -> usize {
        match tier {
            Tier::Small => self.small,
            Tier::Medium => self.medium,
            Tier::Large => self.large,
        }
        .max(1)
    }

    /// The smaller of each tier: for work that reads one disk and writes another.
    pub fn min(self, other: Self) -> Self {
        Self { small: self.small.min(other.small), medium: self.medium.min(other.medium), large: self.large.min(other.large) }
    }

    /// Every tier the same: `n` files at once whatever their size (a CLI's `--jobs n`).
    pub fn uniform(n: usize) -> Self {
        let n = n.max(1);
        Self { small: n, medium: n, large: n }
    }
}

/// Auto's numbers for work reading `path`'s disk with `flags` and `stages`.
pub fn auto_limits(path: &Path, flags: HashFlags, stages: JobStages) -> Limits {
    auto_limits_on(StorageKind::for_path(path), flags, stages)
}

/// Auto's numbers for work reading a disk of kind `storage`: what [`auto_limits`] picks once
/// the disk is known, or when the user said what it is.
pub fn auto_limits_on(storage: StorageKind, flags: HashFlags, stages: JobStages) -> Limits {
    auto_limits_for(storage, RamClass::detect(), super::native_lib::physical_core_count(), flags, stages)
}

fn auto_limits_for(storage: StorageKind, ram: RamClass, cores: usize, flags: HashFlags, stages: JobStages) -> Limits {
    let small = recommended_parallel_files(FileSizeClass::Small, storage, ram, cores, flags, stages);
    let large = recommended_parallel_files(FileSizeClass::Large, storage, ram, cores, flags, stages);
    Limits { small, medium: large, large: large.min(AUTO_LARGE_MAX) }
}

/// One semaphore per tier. tokio's semaphore is fair, so files of one tier start in the order
/// they asked.
#[derive(Clone)]
pub struct Gate {
    tiers: [Arc<Semaphore>; 3],
    pub limits: Limits,
}

impl Gate {
    pub fn new(limits: Limits) -> Self {
        Self { tiers: [Tier::Small, Tier::Medium, Tier::Large].map(|t| Arc::new(Semaphore::new(limits.get(t)))), limits }
    }

    /// Waits for a slot in `tier`; the slot is held until the permit is dropped.
    pub async fn enter(&self, tier: Tier) -> OwnedSemaphorePermit {
        Arc::clone(&self.tiers[tier.index()]).acquire_owned().await.expect("the gate's semaphores are never closed")
    }

    /// Files of every tier running right now.
    pub fn running(&self) -> usize {
        [Tier::Small, Tier::Medium, Tier::Large].iter().map(|&t| self.limits.get(t) - self.tiers[t.index()].available_permits()).sum()
    }
}

/// One input of a [`Multiple`] run: what to run [`Single`] on, and the caller's own `tag`
/// (an id, a planned copy) handed back with the result.
pub struct Job<T> {
    pub input: PathBuf,
    pub args: SingleArgs,
    /// Bytes this job reads: picks its tier and weighs it in the shared progress.
    pub size: u64,
    pub tag: T,
}

/// Reports a job's own bytes done; returns `false` once the run is cancelled.
pub type OnBytes<'a> = &'a mut dyn FnMut(u64) -> bool;

/// One file being worked on right now: what a screen lists as "reading these".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunningFile {
    /// What the caller named it (a path).
    pub name: String,
    pub size: u64,
    pub done: u64,
    /// What it is doing: `detect`, `extract`, `hash`, `convert`, `archive` (a [`Single`]'s
    /// stage), or empty for a job that does not say.
    pub stage: String,
}

/// A running job's handle on its own progress: bytes done, and (for a [`Single`]) its stage.
pub struct Tracker<'a> {
    on_bytes: &'a mut dyn FnMut(u64) -> bool,
    entry: Option<(u64, Arc<Mutex<BTreeMap<u64, RunningFile>>>)>,
}

impl Tracker<'_> {
    /// Bytes done so far (cumulative, `0..=size`); `false` once the run is cancelled.
    pub fn bytes(&mut self, done: u64) -> bool {
        if let Some((seq, running)) = &self.entry {
            if let Some(file) = running.lock().unwrap_or_else(|p| p.into_inner()).get_mut(seq) {
                file.done = done.min(file.size);
            }
        }
        (self.on_bytes)(done)
    }

    pub fn stage(&mut self, stage: &str) {
        if let Some((seq, running)) = &self.entry {
            if let Some(file) = running.lock().unwrap_or_else(|p| p.into_inner()).get_mut(seq) {
                if file.stage != stage {
                    file.stage = stage.to_string();
                }
            }
        }
    }
}

/// A finished job's result, as it comes out of [`Multiple::run`].
pub type Finished<T, R> = Pin<Box<dyn Future<Output = (T, R)> + Send>>;

/// Runs jobs several at a time. Cloning shares the gate, the progress and the cancel flag.
///
/// Jobs start on the current tokio runtime as soon as they are submitted, each tier's in the
/// order given; dropping the returned set does not stop them -- [`Multiple::cancel`] does.
#[derive(Clone)]
pub struct Multiple {
    gate: Gate,
    /// Across every tier at once, when set ([`Multiple::with_total`]).
    total_cap: Option<Arc<Semaphore>>,
    cancel: Arc<AtomicBool>,
    done: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
    /// The files being worked on right now, by start order.
    running: Arc<Mutex<BTreeMap<u64, RunningFile>>>,
    next_seq: Arc<AtomicU64>,
    /// The folder and size a [`Multiple::run_in_turn`] run is on.
    segment: Arc<Mutex<Option<(String, Tier)>>>,
}

impl Multiple {
    pub fn new(limits: Limits) -> Self {
        Self {
            gate: Gate::new(limits),
            total_cap: None,
            cancel: Arc::new(AtomicBool::new(false)),
            done: Arc::new(AtomicU64::new(0)),
            total: Arc::new(AtomicU64::new(0)),
            running: Arc::new(Mutex::new(BTreeMap::new())),
            next_seq: Arc::new(AtomicU64::new(0)),
            segment: Arc::new(Mutex::new(None)),
        }
    }

    /// The files being worked on right now, in the order they started, each with its own
    /// progress.
    pub fn running_files(&self) -> Vec<RunningFile> {
        self.running.lock().unwrap_or_else(|p| p.into_inner()).values().cloned().collect()
    }

    /// Also at most `n` jobs at once whatever their tiers -- for a caller whose limit is one
    /// number (chdlite's `--jobs`), where the tiers' own limits alone would allow one per tier.
    pub fn with_total(mut self, n: usize) -> Self {
        self.total_cap = Some(Arc::new(Semaphore::new(n.max(1))));
        self
    }

    pub fn limits(&self) -> Limits {
        self.gate.limits
    }

    /// Stops every running job at its next progress check and starts no more.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// Bytes done over every job so far (finished ones whole, running ones as far as they
    /// got), the total, and how many are running.
    pub fn progress(&self) -> (u64, u64, usize) {
        (self.done.load(Ordering::Relaxed), self.total.load(Ordering::Relaxed), self.gate.running())
    }

    /// Runs one [`Single`] per job. Results arrive as each finishes (poll the returned set as
    /// a `Stream`); a cancelled job comes back `Err(SingleError::Cancelled)`.
    pub fn run<T: Send + 'static>(&self, jobs: Vec<Job<T>>) -> FuturesUnordered<Finished<T, Result<SingleOutcome, SingleError>>> {
        let items = jobs.into_iter().map(|job| (job.size, job.input.display().to_string(), (job.input, job.args, job.size, job.tag))).collect();
        self.run_tracked(items, run_single).map_outputs()
    }

    /// The general form: `work` is any blocking job over one item, reporting its own bytes
    /// done through `on_bytes` (cumulative, `0..=size`). For a job that is not one
    /// [`Single`] -- a plain byte copy beside the conversions of the same copy job -- so every
    /// file of a job still shares one gate, one progress and one cancel.
    pub fn run_blocking<I, R, W>(&self, items: Vec<(u64, I)>, work: W) -> FuturesUnordered<Finished<(), R>>
    where
        I: Send + 'static,
        R: Send + 'static,
        W: Fn(I, OnBytes<'_>) -> R + Send + Sync + 'static,
    {
        self.run_tracked(items.into_iter().map(|(size, item)| (size, String::new(), item)).collect(), move |item, tracker| work(item, &mut |n| tracker.bytes(n)))
    }

    /// [`Self::run_blocking`] with a name per item, listed in [`Self::running_files`] while
    /// it runs, and a [`Tracker`] that can also say what stage the job is in.
    pub fn run_tracked<I, R, W>(&self, items: Vec<(u64, String, I)>, work: W) -> FuturesUnordered<Finished<(), R>>
    where
        I: Send + 'static,
        R: Send + 'static,
        W: Fn(I, &mut Tracker<'_>) -> R + Send + Sync + 'static,
    {
        let starter = self.starter(work);
        let set = FuturesUnordered::new();
        // One feeder per tier starts that tier's jobs in the order given, each as a task of
        // its own as soon as it has its slots -- so files of one size start in submission
        // order, and a large file waiting for its slot never holds up the small ones.
        let mut by_tier: [Vec<(u64, String, I, tokio::sync::oneshot::Sender<R>)>; 3] = Default::default();
        for (size, name, item) in items {
            let tx = self.listen(&set, size);
            by_tier[Tier::of(size).index()].push((size, name, item, tx));
        }
        for (tier, jobs) in [Tier::Small, Tier::Medium, Tier::Large].into_iter().zip(by_tier) {
            if jobs.is_empty() {
                continue;
            }
            let starter = starter.clone();
            tokio::spawn(async move {
                for (size, name, item, tx) in jobs {
                    starter.start(tier, size, name, item, tx).await;
                }
            });
        }
        set
    }

    /// [`Self::run`] one group and one size at a time: the jobs of a group (a scan's
    /// first-level folder) run before the next group's, and inside a group every small file,
    /// then every medium one, then every large one, smallest first -- each size at its own
    /// limit, never two sizes at once. For reading one hard disk: two big sequential reads
    /// and a stream of small ones together only make the head seek between them, and a
    /// folder is finished, and can be matched, before the next is started.
    ///
    /// The price is a slot or two idle while the last files of one size finish before the
    /// next size starts; across a library that is a few seconds per folder.
    pub fn run_in_turn<T: Send + 'static>(&self, jobs: Vec<(String, Job<T>)>) -> FuturesUnordered<Finished<T, Result<SingleOutcome, SingleError>>> {
        let items = jobs.into_iter().map(|(group, job)| (group, job.size, job.input.display().to_string(), (job.input, job.args, job.size, job.tag))).collect();
        self.run_tracked_in_turn(items, run_single).map_outputs()
    }

    /// [`Self::run_tracked`] in [`Self::run_in_turn`]'s order: `(group, size, name, item)`.
    pub fn run_tracked_in_turn<I, R, W>(&self, items: Vec<(String, u64, String, I)>, work: W) -> FuturesUnordered<Finished<(), R>>
    where
        I: Send + 'static,
        R: Send + 'static,
        W: Fn(I, &mut Tracker<'_>) -> R + Send + Sync + 'static,
    {
        let starter = self.starter(work);
        let set = FuturesUnordered::new();
        let mut queue = Vec::with_capacity(items.len());
        for (group, size, name, item) in items {
            let tx = self.listen(&set, size);
            queue.push((group, size, name, item, tx));
        }
        // By group, then size tier, then size: smallest first within a folder.
        queue.sort_by(|a, b| (&a.0, Tier::of(a.1).index(), a.1).cmp(&(&b.0, Tier::of(b.1).index(), b.1)));
        let segment = Arc::clone(&self.segment);
        tokio::spawn(async move {
            let mut in_flight: Vec<tokio::task::JoinHandle<()>> = Vec::new();
            let mut current: Option<(String, Tier)> = None;
            for (group, size, name, item, tx) in queue {
                let tier = Tier::of(size);
                if current.as_ref().is_none_or(|(g, t)| *g != group || *t != tier) {
                    // Everything of the previous folder or size is finished before this
                    // one starts.
                    for handle in in_flight.drain(..) {
                        let _ = handle.await;
                    }
                    *segment.lock().unwrap_or_else(|p| p.into_inner()) = Some((group.clone(), tier));
                    current = Some((group, tier));
                }
                in_flight.retain(|h| !h.is_finished());
                in_flight.push(starter.start(tier, size, name, item, tx).await);
            }
        });
        set
    }

    /// The folder and size [`Self::run_in_turn`] is working on now.
    pub fn current_group(&self) -> Option<(String, Tier)> {
        self.segment.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Counts `size` into the total and adds the job's result to `set`; the job sends its
    /// result down the returned channel.
    fn listen<R: Send + 'static>(&self, set: &FuturesUnordered<Finished<(), R>>, size: u64) -> tokio::sync::oneshot::Sender<R> {
        self.total.fetch_add(size, Ordering::Relaxed);
        let (tx, rx) = tokio::sync::oneshot::channel();
        set.push(Box::pin(async move {
            match rx.await {
                Ok(result) => ((), result),
                // The job's thread panicked; the panic itself was already reported there.
                Err(_) => panic!("a Multiple job panicked"),
            }
        }) as Finished<(), R>);
        tx
    }

    fn starter<I, R, W>(&self, work: W) -> Starter<I, R, W> {
        Starter {
            gate: self.gate.clone(),
            total_cap: self.total_cap.clone(),
            cancel: Arc::clone(&self.cancel),
            done: Arc::clone(&self.done),
            running: Arc::clone(&self.running),
            next_seq: Arc::clone(&self.next_seq),
            work: Arc::new(work),
            types: std::marker::PhantomData,
        }
    }
}

/// What a feeder needs to start one job: the run's gate, caps, progress and work.
struct Starter<I, R, W> {
    gate: Gate,
    total_cap: Option<Arc<Semaphore>>,
    cancel: Arc<AtomicBool>,
    done: Arc<AtomicU64>,
    running: Arc<Mutex<BTreeMap<u64, RunningFile>>>,
    next_seq: Arc<AtomicU64>,
    work: Arc<W>,
    types: std::marker::PhantomData<fn(I) -> R>,
}

impl<I, R, W> Clone for Starter<I, R, W> {
    fn clone(&self) -> Self {
        Self {
            gate: self.gate.clone(),
            total_cap: self.total_cap.clone(),
            cancel: Arc::clone(&self.cancel),
            done: Arc::clone(&self.done),
            running: Arc::clone(&self.running),
            next_seq: Arc::clone(&self.next_seq),
            work: Arc::clone(&self.work),
            types: std::marker::PhantomData,
        }
    }
}

impl<I, R, W> Starter<I, R, W>
where
    I: Send + 'static,
    R: Send + 'static,
    W: Fn(I, &mut Tracker<'_>) -> R + Send + Sync + 'static,
{
    /// Waits for the job's slots, then runs it on a blocking thread of its own, so it frees
    /// its slots when it ends whether or not anyone is polling the results right now. The
    /// handle finishes once the result has been sent.
    async fn start(&self, tier: Tier, size: u64, name: String, item: I, tx: tokio::sync::oneshot::Sender<R>) -> tokio::task::JoinHandle<()> {
        // Always the overall cap first, then the tier: one order everywhere, so two feeders
        // never each hold the slot the other is waiting for.
        let overall = match &self.total_cap {
            Some(cap) => Some(Arc::clone(cap).acquire_owned().await.expect("the cap is never closed")),
            None => None,
        };
        let slot = self.gate.enter(tier).await;
        let (cancel, done, work) = (Arc::clone(&self.cancel), Arc::clone(&self.done), Arc::clone(&self.work));
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
        let listed = !name.is_empty();
        if listed {
            self.running.lock().unwrap_or_else(|p| p.into_inner()).insert(seq, RunningFile { name, size, done: 0, stage: String::new() });
        }
        let running = Arc::clone(&self.running);
        tokio::task::spawn_blocking(move || {
            let mut counted = 0u64;
            let mut on_bytes = |now: u64| {
                let now = now.min(size);
                if now > counted {
                    done.fetch_add(now - counted, Ordering::Relaxed);
                    counted = now;
                }
                !cancel.load(Ordering::Relaxed)
            };
            let mut tracker = Tracker { on_bytes: &mut on_bytes, entry: listed.then(|| (seq, Arc::clone(&running))) };
            let result = work(item, &mut tracker);
            drop(tracker);
            if listed {
                running.lock().unwrap_or_else(|p| p.into_inner()).remove(&seq);
            }
            done.fetch_add(size - counted, Ordering::Relaxed);
            drop((slot, overall));
            let _ = tx.send(result);
        })
    }
}

/// One [`Single`] over a job: what [`Multiple::run`] and [`Multiple::run_in_turn`] run, and
/// what a caller's own [`Multiple::run_tracked_in_turn`] work can start with.
pub fn run_single<T>((input, args, size, tag): (PathBuf, SingleArgs, u64, T), tracker: &mut Tracker<'_>) -> (T, Result<SingleOutcome, SingleError>) {
    let result = Single::new(input, args).run(&mut |event: &ProgressEvent| {
        tracker.stage(stage_name(event.stage));
        tracker.bytes((event.total_fraction as f64 * size as f64) as u64)
    });
    (tag, result)
}

fn stage_name(stage: super::operation::Stage) -> &'static str {
    use super::operation::Stage;
    match stage {
        Stage::Detect => "detect",
        Stage::Extract => "extract",
        Stage::Hash => "hash",
        Stage::Convert => "convert",
        Stage::Archive => "archive",
    }
}

/// Unwraps the `((), (tag, result))` shape `run` builds on `run_blocking`.
trait MapOutputs<T, R> {
    fn map_outputs(self) -> FuturesUnordered<Finished<T, R>>;
}

impl<T: Send + 'static, R: Send + 'static> MapOutputs<T, R> for FuturesUnordered<Finished<(), (T, R)>> {
    fn map_outputs(self) -> FuturesUnordered<Finished<T, R>> {
        self.into_iter()
            .map(|fut| -> Finished<T, R> {
                Box::pin(async move {
                    let ((), out) = fut.await;
                    out
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;

    #[test]
    fn tiers_split_at_16_mib_and_1_gib() {
        assert_eq!(Tier::of(0), Tier::Small);
        assert_eq!(Tier::of(16 << 20), Tier::Small);
        assert_eq!(Tier::of((16 << 20) + 1), Tier::Medium);
        assert_eq!(Tier::of((1 << 30) - 1), Tier::Medium);
        assert_eq!(Tier::of(1 << 30), Tier::Large);
    }

    #[test]
    fn auto_scales_with_the_machine_and_keeps_large_files_few() {
        let big = auto_limits_for(StorageKind::Ssd, RamClass::Large, 32, HashFlags::CRC32, JobStages::default());
        assert!(big.small >= 8, "{big:?}");
        assert!(big.large <= AUTO_LARGE_MAX, "{big:?}");
        let hdd = auto_limits_for(StorageKind::Hdd, RamClass::Large, 32, HashFlags::CRC32, JobStages::default());
        assert_eq!((hdd.small, hdd.medium, hdd.large), (2, 1, 1), "a spinning disk wants one big read at a time");
    }

    #[tokio::test]
    async fn the_gate_holds_each_tier_to_its_own_limit() {
        let gate = Gate::new(Limits { small: 2, medium: 1, large: 1 });
        let a = gate.enter(Tier::Small).await;
        let _b = gate.enter(Tier::Small).await;
        let _l = gate.enter(Tier::Large).await;
        assert_eq!(gate.running(), 3);
        assert!(tokio::time::timeout(std::time::Duration::from_millis(50), gate.enter(Tier::Small)).await.is_err(), "a third small file waits");
        drop(a);
        let _c = tokio::time::timeout(std::time::Duration::from_millis(50), gate.enter(Tier::Small)).await.expect("and gets in once one leaves");
    }

    /// No more than the tier's limit run at once, results come back as they finish (not in
    /// submission order), and progress ends at the total.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn runs_up_to_the_limit_and_streams_results_as_they_finish() {
        let multiple = Multiple::new(Limits::uniform(2));
        let peak = Arc::new(AtomicU64::new(0));
        let now = Arc::new(AtomicU64::new(0));
        let (p, n) = (Arc::clone(&peak), Arc::clone(&now));
        // Item 0 is slow, so later ones overtake it.
        let items: Vec<(u64, u64)> = (0..6u64).map(|i| (100, i)).collect();
        let mut set = multiple.run_blocking(items, move |i, on_bytes| {
            let running = n.fetch_add(1, Ordering::SeqCst) + 1;
            p.fetch_max(running, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(if i == 0 { 300 } else { 30 }));
            on_bytes(50);
            n.fetch_sub(1, Ordering::SeqCst);
            i
        });
        let mut order = Vec::new();
        while let Some(((), i)) = set.next().await {
            order.push(i);
        }
        assert_eq!(peak.load(Ordering::SeqCst), 2, "never more than the limit");
        assert_ne!(order[0], 0, "the slow first item does not hold up the others: {order:?}");
        assert_eq!(order.len(), 6);
        assert_eq!(multiple.progress().0, 600);
        assert_eq!(multiple.progress().1, 600);
    }

    /// What is running is listed by name with its own progress, and leaves the list when it
    /// ends.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn running_files_lists_what_is_in_flight() {
        let multiple = Multiple::new(Limits::uniform(2));
        let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
        let gate_rx = Arc::new(Mutex::new(gate_rx));
        let items = vec![(100u64, "a.bin".to_string(), ()), (100u64, "b.bin".to_string(), ())];
        let mut set = multiple.run_tracked(items, move |(), tracker| {
            tracker.stage("hash");
            tracker.bytes(40);
            let _ = gate_rx.lock().unwrap().recv();
        });
        let mut seen = Vec::new();
        for _ in 0..100 {
            seen = multiple.running_files();
            if seen.len() == 2 && seen.iter().all(|f| f.done == 40) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let mut names: Vec<_> = seen.iter().map(|f| f.name.clone()).collect();
        names.sort();
        assert_eq!(names, vec!["a.bin", "b.bin"]);
        assert!(seen.iter().all(|f| f.stage == "hash" && f.size == 100 && f.done == 40), "{seen:?}");
        gate_tx.send(()).unwrap();
        gate_tx.send(()).unwrap();
        while set.next().await.is_some() {}
        assert!(multiple.running_files().is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_total_cap_holds_across_tiers() {
        let multiple = Multiple::new(Limits::uniform(4)).with_total(1);
        let peak = Arc::new(AtomicU64::new(0));
        let now = Arc::new(AtomicU64::new(0));
        let (p, n) = (Arc::clone(&peak), Arc::clone(&now));
        // One of each tier: without the cap all three would run together.
        let items = vec![(1u64, ()), (64u64 << 20, ()), (2u64 << 30, ())];
        let mut set = multiple.run_blocking(items, move |(), _| {
            let running = n.fetch_add(1, Ordering::SeqCst) + 1;
            p.fetch_max(running, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(20));
            n.fetch_sub(1, Ordering::SeqCst);
        });
        while set.next().await.is_some() {}
        assert_eq!(peak.load(Ordering::SeqCst), 1);
    }

    /// One at a time, files of one tier run in the order given -- what a "read these in
    /// order" caller relies on.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn one_tier_starts_in_submission_order() {
        let multiple = Multiple::new(Limits::uniform(1));
        let started = Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = Arc::clone(&started);
        let mut set = multiple.run_blocking((0..20u32).map(|i| (10u64, i)).collect(), move |i, _| {
            log.lock().unwrap().push(i);
            std::thread::sleep(std::time::Duration::from_millis(2));
        });
        while set.next().await.is_some() {}
        assert_eq!(*started.lock().unwrap(), (0..20).collect::<Vec<_>>());
    }

    /// One group, then the next; in a group small, then medium, then large, smallest first;
    /// never two sizes at once, and never more than a size's own limit.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn in_turn_runs_one_folder_and_one_size_at_a_time() {
        let multiple = Multiple::new(Limits { small: 3, medium: 2, large: 1 });
        let (mid, big) = (SMALL_MAX_BYTES + 1, MEDIUM_MAX_BYTES);
        // Given out of order on purpose.
        let items: Vec<(String, u64, String, u64)> = vec![
            ("b".into(), 5, String::new(), 5),
            ("a".into(), big, String::new(), big),
            ("a".into(), 3, String::new(), 3),
            ("a".into(), mid + 2, String::new(), mid + 2),
            ("a".into(), 1, String::new(), 1),
            ("a".into(), 2, String::new(), 2),
            ("a".into(), mid + 1, String::new(), mid + 1),
            ("a".into(), 4, String::new(), 4),
        ];
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let live: Arc<std::sync::Mutex<Vec<Tier>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mixed = Arc::new(AtomicBool::new(false));
        let (log2, live2, mixed2) = (Arc::clone(&log), Arc::clone(&live), Arc::clone(&mixed));
        let mut set = multiple.run_tracked_in_turn(items, move |size: u64, _: &mut Tracker<'_>| {
            let tier = Tier::of(size);
            {
                let mut now = live2.lock().unwrap();
                now.push(tier);
                let limit = Limits { small: 3, medium: 2, large: 1 }.get(tier);
                if now.iter().any(|t| *t != tier) || now.len() > limit {
                    mixed2.store(true, Ordering::SeqCst);
                }
            }
            log2.lock().unwrap().push(size);
            std::thread::sleep(std::time::Duration::from_millis(15));
            let mut now = live2.lock().unwrap();
            let at = now.iter().position(|t| *t == tier).unwrap();
            now.remove(at);
        });
        while set.next().await.is_some() {}
        assert!(!mixed.load(Ordering::SeqCst), "two sizes, or more than a size's limit, ran at once");
        let started = log.lock().unwrap().clone();
        // Folder a's small files (in some order among themselves, 3 at once), then its medium
        // ones, then its large one, then folder b.
        let mut smalls = started[..4].to_vec();
        smalls.sort();
        assert_eq!(smalls, vec![1, 2, 3, 4], "{started:?}");
        let mut mediums = started[4..6].to_vec();
        mediums.sort();
        assert_eq!(mediums, vec![mid + 1, mid + 2], "{started:?}");
        assert_eq!(&started[6..], &[big, 5], "{started:?}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_reaches_running_jobs() {
        let multiple = Multiple::new(Limits::uniform(1));
        let handle = multiple.clone();
        let mut set = multiple.run_blocking(vec![(10, ())], move |(), on_bytes| {
            let mut ticks = 0;
            while on_bytes(1) {
                ticks += 1;
                std::thread::sleep(std::time::Duration::from_millis(5));
                if ticks > 1000 {
                    return false;
                }
            }
            true
        });
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        handle.cancel();
        let ((), stopped) = set.next().await.unwrap();
        assert!(stopped, "the job saw the cancel");
    }
}
