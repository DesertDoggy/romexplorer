//! Many inputs, several at a time.
//!
//! chdman and dolphinrvz each allow one conversion per *process* (romexplorer serialises them
//! behind `CHDMAN_CALL_LOCK` / `DOLPHINRVZ_CALL_LOCK`: chdman's compressor pool is global), so
//! running files side by side means one worker process per file. A worker is this same
//! executable started with `--job <spec>`; it reports on stdout, one JSON [`WorkerLine`] per
//! line. Killing a worker is also the only way to stop a CHD creation part-way, which is why
//! the GUI uses workers even for a single file.
//!
//! Android has no executable to start again (an app is a library inside the system's own
//! process), so there every job runs in this process, one file at a time. Cancel then stops
//! everything but a CHD creation, which finishes its file first.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use romexplorer::rom::multiple::{Limits, Multiple};

use serde::{Deserialize, Serialize};

use crate::job::{remove_outputs, run_one, Event, JobSpec, Mode};
use crate::settings::Settings;

pub const WORKER_FLAG: &str = "--job";

/// Whether this platform can run a job in a worker process (see the module comment).
pub const CAN_SPAWN_WORKERS: bool = !cfg!(target_os = "android");

#[derive(Debug, Serialize, Deserialize)]
enum WorkerLine {
    Event(Event),
    Done(Result<String, String>),
}

#[derive(Debug, Clone)]
pub enum BatchEvent {
    Started { index: usize },
    Progress { index: usize, percent: f32 },
    Message { index: usize, text: String },
    Finished { index: usize, result: Result<String, String> },
}

pub struct Batch {
    pub mode: Mode,
    pub inputs: Vec<PathBuf>,
    pub settings: Settings,
    /// Run every job in a worker process even when there is only one at a time.
    pub always_workers: bool,
}

/// Runs the batch, blocking until every job has finished or been cancelled. `on_event` is
/// called on this thread. Returns how many jobs failed.
///
/// The jobs are scheduled by romexplorer's [`Multiple`] -- the runner its scans and copies use
/// -- on a tokio runtime of their own: several at a time, each job on a blocking thread of its
/// own (which, for a conversion, starts and supervises a worker process).
pub fn run(batch: &Batch, cancel: &AtomicBool, on_event: &(dyn Fn(BatchEvent) + Sync)) -> usize {
    // Reading a header takes no time and its output is meant to be read in order.
    let (concurrent, threads) = if batch.mode == Mode::Read || !CAN_SPAWN_WORKERS { (1, 0) } else { batch.settings.jobs.plan(batch.inputs.len()) };
    let workers = CAN_SPAWN_WORKERS && batch.mode != Mode::Read && (batch.always_workers || concurrent > 1);
    let spec = |index: usize| JobSpec {
        mode: batch.mode,
        input: batch.inputs[index].clone(),
        settings: batch.settings.clone(),
        threads: (threads > 0 && (concurrent > 1 || batch.settings.jobs.threads > 0)).then_some(threads),
    };
    // Every job one size: chdlite's limit is one number of files, not a number per size.
    let items: Vec<(u64, (usize, JobSpec))> = (0..batch.inputs.len()).map(|index| (1, (index, spec(index)))).collect();

    let stop = Arc::new(AtomicBool::new(false));
    let children: Arc<Mutex<HashMap<usize, Child>>> = Arc::new(Mutex::new(HashMap::new()));
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(err) => {
            for index in 0..batch.inputs.len() {
                on_event(BatchEvent::Finished { index, result: Err(format!("cannot start: {err}")) });
            }
            return batch.inputs.len();
        }
    };
    runtime.block_on(async {
        use futures_util::StreamExt;
        let multiple = Multiple::new(Limits::uniform(concurrent)).with_total(concurrent);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<BatchEvent>();
        let (job_stop, job_children) = (Arc::clone(&stop), Arc::clone(&children));
        let mut running = multiple.run_blocking(items, move |(index, spec): (usize, JobSpec), _| {
            if job_stop.load(Ordering::Relaxed) {
                // Reported as cancelled with the jobs that never started, below.
                return None;
            }
            let send = |event: BatchEvent| {
                let _ = tx.send(event);
            };
            send(BatchEvent::Started { index });
            let result = if workers { run_worker(index, &spec, &job_children, &job_stop, &send) } else { run_here(index, &spec, &job_stop, &send) };
            let failed = result.is_err();
            send(BatchEvent::Finished { index, result });
            Some(failed)
        });
        let mut failed = 0;
        let mut finished = vec![false; batch.inputs.len()];
        let mut pass = |event: BatchEvent| {
            if let BatchEvent::Finished { index, .. } = &event {
                if let Some(seen) = finished.get_mut(*index) {
                    *seen = true;
                }
            }
            on_event(event);
        };
        // One timer for the whole loop: a sleep made afresh each turn never fires while events
        // keep arriving, and the cancel would never be seen.
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(100));
        loop {
            tokio::select! {
                Some(event) = rx.recv() => pass(event),
                next = running.next() => match next {
                    Some(((), Some(true))) => failed += 1,
                    Some(_) => {}
                    None => break,
                },
                // The workers block on their children's output, so stopping them is this loop's job.
                _ = tick.tick() => {
                    if cancel.load(Ordering::Relaxed) {
                        stop.store(true, Ordering::Relaxed);
                        multiple.cancel();
                        for child in children.lock().unwrap().values_mut() {
                            let _ = child.kill();
                        }
                    }
                }
            }
        }
        while let Ok(event) = rx.try_recv() {
            pass(event);
        }
        // Jobs a cancel stopped before they started: counted, so the totals add up.
        for index in finished.iter().enumerate().filter(|(_, done)| !**done).map(|(index, _)| index).collect::<Vec<_>>() {
            on_event(BatchEvent::Finished { index, result: Err("cancelled".to_string()) });
            failed += 1;
        }
        failed
    })
}

fn forward(index: usize, event: Event, on_event: &dyn Fn(BatchEvent)) {
    match event {
        Event::Progress(percent) => on_event(BatchEvent::Progress { index, percent }),
        Event::Message(text) => on_event(BatchEvent::Message { index, text }),
        Event::Output(_) => {}
    }
}

fn run_here(index: usize, spec: &JobSpec, cancel: &AtomicBool, on_event: &dyn Fn(BatchEvent)) -> Result<String, String> {
    run_one(spec, &mut |event| {
        forward(index, event, on_event);
        !cancel.load(Ordering::Relaxed)
    })
}

fn run_worker(index: usize, spec: &JobSpec, children: &Mutex<HashMap<usize, Child>>, cancel: &AtomicBool, on_event: &dyn Fn(BatchEvent)) -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|err| format!("cannot find own executable: {err}"))?;
    let json = serde_json::to_string(spec).map_err(|err| err.to_string())?;
    let mut command = Command::new(exe);
    // The worker's stdin is a pipe nobody writes to: it closes when this process ends, however
    // it ends (the window closed, Ctrl-C, a crash), and the worker then stops too.
    command.arg(WORKER_FLAG).arg(json).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: a worker of the GUI must not flash a console of its own.
        command.creation_flags(0x0800_0000);
    }
    let mut child = command.spawn().map_err(|err| format!("cannot start worker: {err}"))?;
    let stdout = child.stdout.take().expect("piped stdout");
    children.lock().unwrap().insert(index, child);

    let mut outputs = Vec::new();
    let mut result: Option<Result<String, String>> = None;
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        match serde_json::from_str::<WorkerLine>(&line) {
            Ok(WorkerLine::Event(event)) => {
                if let Event::Output(path) = &event {
                    outputs.push(path.clone());
                }
                forward(index, event, on_event);
            }
            Ok(WorkerLine::Done(done)) => result = Some(done),
            Err(_) => {}
        }
    }
    if let Some(mut child) = children.lock().unwrap().remove(&index) {
        let _ = child.wait();
    }
    match result {
        Some(result) => result,
        // Killed, or it died: whatever it was writing is not a finished file.
        None => {
            remove_outputs(&outputs);
            Err(if cancel.load(Ordering::Relaxed) { "cancelled".to_string() } else { "worker process ended unexpectedly".to_string() })
        }
    }
}

/// The body of a worker process: `<exe> --job <json>`. Returns the process exit code.
pub fn worker_main(json: &str) -> i32 {
    let emit = |line: &WorkerLine| {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{}", serde_json::to_string(line).unwrap_or_default());
        let _ = out.flush();
    };
    // What this worker is writing, for the watcher below to remove.
    static WRITING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
    std::thread::spawn(|| {
        use std::io::Read;
        let mut sink = [0u8; 64];
        // Only end-of-file (or an error) ends this: the supervisor has gone.
        while matches!(std::io::stdin().read(&mut sink), Ok(n) if n > 0) {}
        remove_outputs(&WRITING.lock().map(|w| w.clone()).unwrap_or_default());
        std::process::exit(2);
    });
    let result = match serde_json::from_str::<JobSpec>(json) {
        Ok(spec) => crate::natives::ensure().and_then(|()| {
            run_one(&spec, &mut |event| {
                if let Event::Output(path) = &event {
                    if let Ok(mut writing) = WRITING.lock() {
                        writing.push(path.clone());
                    }
                }
                emit(&WorkerLine::Event(event));
                true
            })
        }),
        Err(err) => Err(format!("bad job spec: {err}")),
    };
    let code = if result.is_ok() { 0 } else { 1 };
    emit(&WorkerLine::Done(result));
    code
}
