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

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use romexplorer::rom::multiple::{Limits, Multiple};

use serde::{Deserialize, Serialize};

use crate::job::{plan, run_one, Event, JobSpec, Mode, Planned, Writing};
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
    /// Ctrl-C cancels the batch the way the cancel flag does, rather than ending the process
    /// with its outputs half-written (the console tools).
    pub ctrl_c_cancels: bool,
}

/// Runs the batch, blocking until every job has finished or been cancelled. `on_event` is
/// called on this thread. Returns how many jobs failed.
///
/// The jobs are scheduled by romexplorer's [`Multiple`] -- the runner its scans and copies use
/// -- on a tokio runtime of their own: several at a time, each job on a blocking thread of its
/// own (which, for a conversion, starts and supervises a worker process).
pub fn run(batch: &Batch, cancel: &AtomicBool, on_event: &(dyn Fn(BatchEvent) + Sync)) -> usize {
    // Reading a header takes no time and its output is meant to be read in order.
    let (concurrent, threads) = if batch.mode == Mode::Read {
        (1, 0)
    } else if !CAN_SPAWN_WORKERS {
        (1, batch.settings.jobs.plan(1).1)
    } else {
        batch.settings.jobs.plan(batch.inputs.len())
    };
    let workers = CAN_SPAWN_WORKERS && batch.mode != Mode::Read && (batch.always_workers || concurrent > 1);
    let mut specs: Vec<JobSpec> = batch
        .inputs
        .iter()
        .map(|input| JobSpec { mode: batch.mode, input: input.clone(), settings: batch.settings.clone(), threads: (threads > 0).then_some(threads), output_stem: None })
        .collect();
    let clashes = if specs.len() > 1 { separate_outputs(&mut specs) } else { vec![None; specs.len()] };
    // Every job one size: chdlite's limit is one number of files, not a number per size.
    let items: Vec<(u64, (usize, JobSpec, Option<String>))> = specs.into_iter().zip(clashes).enumerate().map(|(index, (spec, clash))| (1, (index, spec, clash))).collect();

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
        let mut running = multiple.run_blocking(items, move |(index, spec, clash): (usize, JobSpec, Option<String>), _| {
            if job_stop.load(Ordering::Relaxed) {
                // Reported as cancelled with the jobs that never started, below.
                return None;
            }
            let send = |event: BatchEvent| {
                let _ = tx.send(event);
            };
            send(BatchEvent::Started { index });
            let result = if let Some(clash) = clash {
                Err(clash)
            } else if workers {
                run_worker(index, &spec, &job_children, &job_stop, &send)
            } else {
                run_here(index, &spec, &job_stop, &send)
            };
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
        let mut ctrl_c = Box::pin(tokio::signal::ctrl_c());
        let (mut watch_ctrl_c, mut interrupted) = (batch.ctrl_c_cancels, false);
        loop {
            tokio::select! {
                pressed = &mut ctrl_c, if watch_ctrl_c => {
                    // Once: the handler stays installed, so a second Ctrl-C does not cut the
                    // clean-up short either.
                    watch_ctrl_c = false;
                    interrupted = pressed.is_ok();
                }
                Some(event) = rx.recv() => pass(event),
                next = running.next() => match next {
                    Some(((), Some(true))) => failed += 1,
                    Some(_) => {}
                    None => break,
                },
                // The workers block on their children's output, so stopping them is this loop's job.
                _ = tick.tick() => {
                    if interrupted || cancel.load(Ordering::Relaxed) {
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
        Event::Output(_) | Event::Scratch(_) => {}
    }
}

/// Gives each job whose output another job of the batch would also write a name of its own
/// (`Game (2).chd`); one whose file names come from the content, which cannot be renamed, gets
/// the error it fails with instead. Earlier inputs keep their names. Names are compared
/// ignoring case, as the file systems that ignore it would.
fn separate_outputs(specs: &mut [JobSpec]) -> Vec<Option<String>> {
    fn key(path: &Path) -> String {
        path.to_string_lossy().to_lowercase()
    }
    // Every output path; every output's folder + stem; the folder + stem of outputs whose
    // extension is not known yet.
    let (mut paths, mut stems, mut unknown) = (HashSet::new(), HashSet::new(), HashSet::new());
    let mut clashes = vec![None; specs.len()];
    for (index, spec) in specs.iter_mut().enumerate() {
        match plan(spec) {
            Planned::Nothing => {}
            Planned::Named { dir, stem, extension } => {
                let taken = |stem: &str| {
                    let base = key(&dir.join(stem));
                    match &extension {
                        Some(ext) => paths.contains(&key(&dir.join(format!("{stem}.{ext}")))) || unknown.contains(&base),
                        None => stems.contains(&base),
                    }
                };
                let free = if taken(&stem) { (2..).map(|n| format!("{stem} ({n})")).find(|candidate| !taken(candidate)) } else { None };
                let stem = match free {
                    Some(free) => {
                        spec.output_stem = Some(free.clone());
                        free
                    }
                    None => stem,
                };
                let base = key(&dir.join(&stem));
                match &extension {
                    Some(ext) => {
                        paths.insert(key(&dir.join(format!("{stem}.{ext}"))));
                    }
                    None => {
                        unknown.insert(base.clone());
                    }
                }
                stems.insert(base);
            }
            Planned::Files(files) => {
                let stem_key = |file: &Path| key(&file.with_extension(""));
                if let Some(file) = files.iter().find(|file| paths.contains(&key(file)) || unknown.contains(&stem_key(file))) {
                    clashes[index] = Some(format!("another file in this batch also writes {} -- give them separate output folders", file.display()));
                    continue;
                }
                for file in &files {
                    paths.insert(key(file));
                    stems.insert(stem_key(file));
                }
            }
        }
    }
    clashes
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

    let mut writing = Writing::default();
    let mut result: Option<Result<String, String>> = None;
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        match serde_json::from_str::<WorkerLine>(&line) {
            Ok(WorkerLine::Event(event)) => {
                writing.note(&event);
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
        // Killed, or it died: whatever it was writing is not a finished file, and a file it
        // was replacing has to come back.
        None => {
            writing.undo();
            Err(if cancel.load(Ordering::Relaxed) { "cancelled".to_string() } else { "worker process ended unexpectedly".to_string() })
        }
    }
}

/// The body of a worker process: `<exe> --job <json>`. Returns the process exit code.
pub fn worker_main(json: &str) -> i32 {
    let emit = |line: &WorkerLine| {
        let mut out = std::io::stdout().lock();
        // On a line of its own even after something a library printed without ending its line.
        let _ = writeln!(out, "\n{}", serde_json::to_string(line).unwrap_or_default());
        let _ = out.flush();
    };
    // What this worker is writing, for the watcher below to undo.
    static WRITING: Mutex<Option<Writing>> = Mutex::new(None);
    std::thread::spawn(|| {
        use std::io::Read;
        let mut sink = [0u8; 64];
        // Only end-of-file (or an error) ends this: the supervisor has gone.
        while matches!(std::io::stdin().read(&mut sink), Ok(n) if n > 0) {}
        if let Some(writing) = WRITING.lock().ok().and_then(|mut w| w.take()) {
            writing.undo();
        }
        std::process::exit(2);
    });
    let result = match serde_json::from_str::<JobSpec>(json) {
        Ok(spec) => crate::natives::ensure().and_then(|()| {
            let result = run_one(&spec, &mut |event| {
                if let Ok(mut writing) = WRITING.lock() {
                    writing.get_or_insert_with(Writing::default).note(&event);
                }
                emit(&WorkerLine::Event(event));
                true
            });
            // Finished (or already undone by `run_one`): nothing left for the watcher to undo.
            if let Ok(mut writing) = WRITING.lock() {
                *writing = None;
            }
            result
        }),
        Err(err) => Err(format!("bad job spec: {err}")),
    };
    let code = if result.is_ok() { 0 } else { 1 };
    emit(&WorkerLine::Done(result));
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(input: PathBuf, settings: &Settings) -> JobSpec {
        JobSpec { mode: Mode::Lite, input, settings: settings.clone(), threads: Some(1), output_stem: None }
    }

    #[test]
    fn two_jobs_writing_one_output_get_names_of_their_own() {
        let dir = std::env::temp_dir().join(format!("chdlite_clash_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for sub in ["A", "B", "out"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        let (a, b) = (dir.join("A").join("Game.cue"), dir.join("B").join("game.cue"));
        std::fs::write(&a, "").unwrap();
        std::fs::write(&b, "").unwrap();
        let mut settings = Settings::default();
        settings.output.dir = dir.join("out").to_string_lossy().into_owned();
        let mut specs = vec![spec(a.clone(), &settings), spec(b.clone(), &settings)];
        assert_eq!(separate_outputs(&mut specs), vec![None, None]);
        assert_eq!(specs[0].output_stem, None);
        assert_eq!(specs[1].output_stem.as_deref(), Some("game (2)"));

        // Each beside its own input: nothing to tell apart.
        let mut specs = vec![spec(a, &Settings::default()), spec(b, &Settings::default())];
        separate_outputs(&mut specs);
        assert!(specs.iter().all(|spec| spec.output_stem.is_none()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
