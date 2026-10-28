//! The background thread that talks to the Editor, so the pane keeps drawing while it waits.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::thread;
use std::time::Instant;

use crate::unity::{Compile, Editor, Mode, RunRequest, RunStatus, TestItem, TestOutcome};

pub enum Job {
    Load(Mode),
    /// Recompile, refresh the listing, then make each request in turn.
    Run {
        mode: Mode,
        requests: Vec<RunRequest>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Checking,
    Compiling,
    Listing,
    /// Another client's async run holds the Editor.
    Waiting,
    Running {
        step: usize,
        of: usize,
    },
}

#[derive(Debug)]
pub enum Update {
    Phase(Phase),
    Listed(Mode, Vec<TestItem>),
    CompileFailed(Vec<String>),
    Results(Mode, Vec<TestOutcome>),
    /// The job is over; every job ends with exactly one.
    Finished(Ending),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Ending {
    Done,
    CompileFailed,
    Cancelled,
    Failed(String),
}

/// Serve jobs until the pane drops its sender. `cancel` stops the job in flight.
pub fn spawn(
    editor: Editor,
    jobs: Receiver<Job>,
    updates: Sender<Update>,
    cancel: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        for job in jobs {
            let ending = match job {
                Job::Load(mode) => load(&editor, mode, &updates),
                Job::Run { mode, requests } => run(&editor, mode, &requests, &updates, &cancel),
            };
            if updates.send(Update::Finished(ending.unwrap_or_else(|ending| ending))).is_err() {
                return;
            }
        }
    });
}

fn load(editor: &Editor, mode: Mode, updates: &Sender<Update>) -> Result<Ending, Ending> {
    let _ = updates.send(Update::Phase(Phase::Checking));
    editor.check().map_err(Ending::Failed)?;
    let _ = updates.send(Update::Phase(Phase::Listing));
    let tests = editor.list_tests(mode).map_err(Ending::Failed)?;
    let _ = updates.send(Update::Listed(mode, tests));
    Ok(Ending::Done)
}

fn run(
    editor: &Editor,
    mode: Mode,
    requests: &[RunRequest],
    updates: &Sender<Update>,
    cancel: &AtomicBool,
) -> Result<Ending, Ending> {
    let cancelled = || cancel.load(Ordering::Relaxed);
    let phase = |phase| {
        let _ = updates.send(Update::Phase(phase));
    };

    phase(Phase::Checking);
    editor.check().map_err(Ending::Failed)?;
    phase(Phase::Compiling);
    if let Compile::Failed(errors) = editor.recompile().map_err(Ending::Failed)? {
        let _ = updates.send(Update::CompileFailed(errors));
        return Ok(Ending::CompileFailed);
    }
    if cancelled() {
        return Ok(Ending::Cancelled);
    }
    phase(Phase::Listing);
    let _ = updates.send(Update::Listed(mode, editor.list_tests(mode).map_err(Ending::Failed)?));

    for (index, request) in requests.iter().enumerate() {
        // Starting a run invalidates whatever run the Editor has in flight, so wait for it.
        while matches!(editor.run_status(), Ok(RunStatus::Running)) {
            if cancelled() {
                return Ok(Ending::Cancelled);
            }
            phase(Phase::Waiting);
            thread::sleep(editor.pace().poll);
        }
        if cancelled() {
            return Ok(Ending::Cancelled);
        }
        phase(Phase::Running { step: index + 1, of: requests.len() });
        editor.start_run(mode, request).map_err(Ending::Failed)?;
        let results = poll_run(editor, cancel)?;
        let _ = updates.send(Update::Results(mode, results));
    }
    Ok(Ending::Done)
}

fn poll_run(editor: &Editor, cancel: &AtomicBool) -> Result<Vec<TestOutcome>, Ending> {
    let mut contact = Instant::now();
    loop {
        thread::sleep(editor.pace().poll);
        if cancel.load(Ordering::Relaxed) {
            return Err(match editor.cancel_run() {
                Ok(()) => Ending::Cancelled,
                Err(error) => Ending::Failed(format!("could not cancel the run: {error}")),
            });
        }
        match editor.run_status() {
            Ok(RunStatus::Running) => contact = Instant::now(),
            Ok(RunStatus::Completed(results)) => return Ok(results),
            Ok(RunStatus::Cancelled) => {
                return Err(Ending::Failed("the run was cancelled in the Editor".into()));
            }
            Ok(RunStatus::Failed(message)) => return Err(Ending::Failed(message)),
            Ok(RunStatus::Idle) => {
                return Err(Ending::Failed("the Editor lost track of the run".into()));
            }
            Err(error) if contact.elapsed() > editor.pace().unreachable => {
                return Err(Ending::Failed(format!("lost contact with the Editor: {error}")));
            }
            Err(_) => {}
        }
    }
}

