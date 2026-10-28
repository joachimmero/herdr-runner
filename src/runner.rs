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

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::time::Duration;

    use serde_json::json;

    use super::*;
    use crate::fake::{FakeEditor, suite};
    use crate::unity::Verdict;

    struct Worker {
        jobs: Sender<Job>,
        updates: Receiver<Update>,
        cancel: Arc<AtomicBool>,
    }

    fn worker(fake: &FakeEditor) -> Worker {
        let (jobs, job_rx) = mpsc::channel();
        let (update_tx, updates) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        spawn(fake.editor(), job_rx, update_tx, Arc::clone(&cancel));
        Worker { jobs, updates, cancel }
    }

    impl Worker {
        /// Every update the job sends, up to and including its `Finished`.
        fn finish(&self, job: Job) -> Vec<Update> {
            self.jobs.send(job).unwrap();
            self.collect(|_| false)
        }

        /// Updates until `Finished`, setting the cancel flag at the first one `cancel_at` picks.
        fn collect(&self, cancel_at: impl Fn(&Update) -> bool) -> Vec<Update> {
            let mut updates = Vec::new();
            loop {
                let update = match self.updates.recv_timeout(Duration::from_secs(20)) {
                    Ok(update) => update,
                    Err(RecvTimeoutError::Timeout) => panic!("no Finished after {updates:?}"),
                    Err(RecvTimeoutError::Disconnected) => panic!("worker gone after {updates:?}"),
                };
                if cancel_at(&update) {
                    self.cancel.store(true, Ordering::Relaxed);
                }
                let finished = matches!(update, Update::Finished(_));
                updates.push(update);
                if finished {
                    return updates;
                }
            }
        }
    }

    fn run(requests: Vec<RunRequest>) -> Job {
        Job::Run { mode: Mode::EditMode, requests }
    }

    fn ending(updates: &[Update]) -> &Ending {
        match updates.last() {
            Some(Update::Finished(ending)) => ending,
            other => panic!("ended without Finished: {other:?}"),
        }
    }

    fn failure(updates: &[Update]) -> &str {
        match ending(updates) {
            Ending::Failed(message) => message,
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    /// The outcomes of each `Results` update, by full name.
    fn results(updates: &[Update]) -> Vec<Vec<(String, Verdict)>> {
        updates
            .iter()
            .filter_map(|update| match update {
                Update::Results(_, outcomes) => Some(
                    outcomes
                        .iter()
                        .map(|outcome| (outcome.full_name.clone(), outcome.verdict))
                        .collect(),
                ),
                _ => None,
            })
            .collect()
    }

    fn phases(updates: &[Update]) -> Vec<Phase> {
        updates
            .iter()
            .filter_map(|update| match update {
                Update::Phase(phase) => Some(*phase),
                _ => None,
            })
            .collect()
    }

    fn passed(name: &str) -> (String, Verdict) {
        (name.to_owned(), Verdict::Passed)
    }

    #[test]
    fn a_load_lists_the_modes_tests_without_compiling() {
        let fake = FakeEditor::new(json!({"tests": suite()}));
        let worker = worker(&fake);

        let updates = worker.finish(Job::Load(Mode::EditMode));
        assert_eq!(ending(&updates), &Ending::Done);
        let Some(Update::Listed(Mode::EditMode, tests)) =
            updates.iter().find(|u| matches!(u, Update::Listed(..)))
        else {
            panic!("no listing in {updates:?}");
        };
        assert_eq!(tests.len(), 5);
        assert!(tests.iter().any(|test| test.full_name == "Game.Sim.RunsForever" && test.explicit));
        assert_eq!(fake.commands(), ["editor_status", "list_tests"]);

        let updates = worker.finish(Job::Load(Mode::PlayMode));
        assert!(
            matches!(&updates[..], [.., Update::Listed(Mode::PlayMode, tests), Update::Finished(Ending::Done)] if tests.len() == 1)
        );
        assert_eq!(fake.calls().last().unwrap()["args"]["mode"], "playmode");
    }

    #[test]
    fn a_fixture_run_compiles_relists_and_runs_only_that_fixture() {
        let fake = FakeEditor::new(json!({"tests": suite(), "run_polls": 3}));
        let updates = worker(&fake).finish(run(vec![RunRequest::fixture("Game.BoardTests")]));

        assert_eq!(ending(&updates), &Ending::Done);
        assert_eq!(
            results(&updates),
            [vec![
                passed("Game.BoardTests.PlacesTile"),
                ("Game.BoardTests.RejectsOverlap".into(), Verdict::Failed)
            ]]
        );
        assert_eq!(
            fake.runs(),
            [json!({"mode": "editor", "filter_type": "testName", "include_explicit": "false",
                    "async_tests": "true", "filter": "Game.BoardTests."})]
        );
        let commands = fake.commands();
        assert_eq!(
            commands[..5],
            ["editor_status", "recompile", "list_tests", "test_status", "run_tests"]
        );
        assert!(commands[5..].iter().all(|command| command == "test_status"));
        assert_eq!(
            phases(&updates),
            [Phase::Checking, Phase::Compiling, Phase::Listing, Phase::Running { step: 1, of: 1 }]
        );
    }

    #[test]
    fn a_failure_carries_its_message_and_stack_trace() {
        let fake = FakeEditor::new(json!({"tests": suite()}));
        let updates = worker(&fake)
            .finish(run(vec![RunRequest::test("Game.BoardTests.RejectsOverlap", false)]));
        let Some(Update::Results(_, outcomes)) =
            updates.iter().find(|u| matches!(u, Update::Results(..)))
        else {
            panic!("no results in {updates:?}");
        };
        assert_eq!(outcomes[0].message.as_deref(), Some("Expected: 1\n  But was: 2"));
        assert!(outcomes[0].stack_trace.as_deref().unwrap().contains("BoardTests.cs:42"));
    }

    #[test]
    fn running_all_skips_explicit_tests() {
        let fake = FakeEditor::new(json!({"tests": suite()}));
        let updates = worker(&fake).finish(run(vec![RunRequest::all()]));
        let names: Vec<String> =
            results(&updates)[0].iter().map(|(name, _)| name.clone()).collect();
        assert_eq!(names.len(), 4);
        assert!(!names.contains(&"Game.Sim.RunsForever".to_owned()));
        assert!(fake.runs()[0].get("filter").is_none());
    }

    #[test]
    fn an_explicit_test_runs_when_asked_for_by_name() {
        let fake = FakeEditor::new(json!({"tests": suite()}));
        let updates =
            worker(&fake).finish(run(vec![RunRequest::test("Game.Sim.RunsForever", true)]));
        assert_eq!(results(&updates), [vec![passed("Game.Sim.RunsForever")]]);
        assert_eq!(fake.runs()[0]["include_explicit"], "true");
    }

    #[test]
    fn an_assembly_run_filters_by_assembly() {
        let fake = FakeEditor::new(json!({"tests": suite()}));
        let updates = worker(&fake).finish(run(vec![RunRequest::assembly("Words.Tests")]));
        assert_eq!(results(&updates), [vec![passed("Words.DictionaryTests.Loads")]]);
        assert_eq!(fake.runs()[0]["filter_type"], "assembly");
    }

    #[test]
    fn several_requests_run_one_after_another() {
        let fake = FakeEditor::new(json!({"tests": suite()}));
        let updates = worker(&fake).finish(run(vec![
            RunRequest::test("Game.BoardTests.PlacesTile", false),
            RunRequest::test("Words.DictionaryTests.Loads", false),
        ]));
        assert_eq!(ending(&updates), &Ending::Done);
        assert_eq!(
            results(&updates),
            [
                vec![passed("Game.BoardTests.PlacesTile")],
                vec![passed("Words.DictionaryTests.Loads")]
            ]
        );
        let running: Vec<Phase> = phases(&updates)
            .into_iter()
            .filter(|phase| matches!(phase, Phase::Running { .. }))
            .collect();
        assert_eq!(running, [Phase::Running { step: 1, of: 2 }, Phase::Running { step: 2, of: 2 }]);
        // One compile serves every request.
        assert_eq!(fake.commands().iter().filter(|command| *command == "recompile").count(), 1);
    }

    #[test]
    fn a_compile_that_fails_reports_its_errors_and_runs_nothing() {
        let errors = ["Assets/Board.cs(3,1): error CS1002: ; expected"];
        let fake = FakeEditor::new(json!({
            "tests": suite(),
            "compile": {"start": "compiling", "polls": 2, "errors": errors},
        }));
        let updates = worker(&fake).finish(run(vec![RunRequest::all()]));
        assert_eq!(ending(&updates), &Ending::CompileFailed);
        assert!(updates.iter().any(|u| matches!(u, Update::CompileFailed(e) if e == &errors)));
        assert!(fake.runs().is_empty());
        assert!(!fake.commands().contains(&"list_tests".to_owned()));
    }

    #[test]
    fn errors_standing_before_the_recompile_also_stop_the_run() {
        let errors = ["Assets/Board.cs(9,4): error CS0103: The name 'x' does not exist"];
        let fake = FakeEditor::new(json!({
            "tests": suite(),
            "compile": {"start": "failed", "errors": errors},
        }));
        let updates = worker(&fake).finish(run(vec![RunRequest::all()]));
        assert_eq!(ending(&updates), &Ending::CompileFailed);
        assert!(updates.iter().any(|u| matches!(u, Update::CompileFailed(e) if e == &errors)));
        assert!(fake.runs().is_empty());
    }

    #[test]
    fn a_clean_compile_is_waited_for_before_the_run() {
        let fake = FakeEditor::new(
            json!({"tests": suite(), "compile": {"start": "compiling", "polls": 3}}),
        );
        let updates = worker(&fake).finish(run(vec![RunRequest::all()]));
        assert_eq!(ending(&updates), &Ending::Done);
        let commands = fake.commands();
        let run_at = commands.iter().position(|command| command == "run_tests").unwrap();
        let polls =
            commands[..run_at].iter().filter(|command| *command == "recompile_status").count();
        assert_eq!(polls, 4);
    }

    #[test]
    fn an_editor_holding_another_project_is_refused() {
        let fake =
            FakeEditor::new(json!({"tests": suite(), "project": "/Users/someone/elsewhere"}));
        let updates = worker(&fake).finish(run(vec![RunRequest::all()]));
        assert!(failure(&updates).contains("/Users/someone/elsewhere"));
        assert_eq!(fake.commands(), ["editor_status"]);
    }

    #[test]
    fn an_unreachable_editor_fails_with_the_clis_message() {
        let fake = FakeEditor::new(json!({"unreachable": true}));
        let updates = worker(&fake).finish(Job::Load(Mode::EditMode));
        assert!(failure(&updates).contains("No Unity Editor instances found"));
    }

    #[test]
    fn another_clients_run_is_waited_out() {
        let fake = FakeEditor::new(json!({"tests": suite(), "foreign_polls": 3}));
        let updates = worker(&fake).finish(run(vec![RunRequest::all()]));
        assert_eq!(ending(&updates), &Ending::Done);
        let phases = phases(&updates);
        let waiting =
            phases.iter().position(|phase| *phase == Phase::Waiting).expect("never waited");
        let running =
            phases.iter().position(|phase| matches!(phase, Phase::Running { .. })).unwrap();
        assert!(waiting < running);
        let commands = fake.commands();
        let run_at = commands.iter().position(|command| command == "run_tests").unwrap();
        assert_eq!(
            commands[..run_at].iter().filter(|command| *command == "test_status").count(),
            4
        );
    }

    #[test]
    fn cancelling_mid_run_cancels_it_in_the_editor() {
        let fake = FakeEditor::new(json!({"tests": suite(), "run_polls": 100_000}));
        let worker = worker(&fake);
        worker.jobs.send(run(vec![RunRequest::all()])).unwrap();
        let updates =
            worker.collect(|update| matches!(update, Update::Phase(Phase::Running { .. })));
        assert_eq!(ending(&updates), &Ending::Cancelled);
        assert!(results(&updates).is_empty());
        assert_eq!(fake.commands().last().unwrap(), "cancel_tests");
    }

    #[test]
    fn cancelling_while_waiting_leaves_the_other_run_alone() {
        let fake = FakeEditor::new(json!({"tests": suite(), "foreign_polls": 100_000}));
        let worker = worker(&fake);
        worker.jobs.send(run(vec![RunRequest::all()])).unwrap();
        let updates = worker.collect(|update| matches!(update, Update::Phase(Phase::Waiting)));
        assert_eq!(ending(&updates), &Ending::Cancelled);
        assert!(fake.runs().is_empty());
        assert!(!fake.commands().contains(&"cancel_tests".to_owned()));
    }

    #[test]
    fn a_refused_run_fails_with_the_editors_error() {
        let fake = FakeEditor::new(json!({"tests": suite(), "refuse_run": "Invalid mode 'x'"}));
        let updates = worker(&fake).finish(run(vec![RunRequest::all()]));
        assert_eq!(failure(&updates), "Invalid mode 'x'");
    }

    #[test]
    fn a_run_that_errors_in_the_editor_fails_with_its_message() {
        let fake =
            FakeEditor::new(json!({"tests": suite(), "run_error": "Tests did not complete"}));
        let updates = worker(&fake).finish(run(vec![RunRequest::all()]));
        assert_eq!(failure(&updates), "Tests did not complete");
    }

    #[test]
    fn the_editor_dropping_out_briefly_mid_run_is_ridden_out() {
        // The first status read is the wait check; the next two land mid-run, as a reload would.
        let fake =
            FakeEditor::new(json!({"tests": suite(), "run_polls": 2, "flaky": {"test_status": 3}}));
        let updates = worker(&fake).finish(run(vec![RunRequest::all()]));
        assert_eq!(ending(&updates), &Ending::Done);
        assert_eq!(results(&updates)[0].len(), 4);
    }

    #[test]
    fn losing_the_editor_for_too_long_fails_the_run() {
        let fake = FakeEditor::new(json!({"tests": suite(), "flaky": {"test_status": 100_000}}));
        let updates = worker(&fake).finish(run(vec![RunRequest::all()]));
        assert!(failure(&updates).starts_with("lost contact with the Editor"));
    }

    #[test]
    fn a_run_cancelled_from_elsewhere_reads_as_a_failure() {
        let fake = FakeEditor::new(json!({"tests": suite(), "run_polls": 100_000}));
        let worker = worker(&fake);
        worker.jobs.send(run(vec![RunRequest::all()])).unwrap();
        // Another client cancels once the run is underway, which is after the phase is reported.
        let cancelled = std::cell::Cell::new(false);
        let updates = worker.collect(|update| {
            if matches!(update, Update::Phase(Phase::Running { .. })) && !cancelled.get() {
                while fake.runs().is_empty() {
                    std::thread::sleep(Duration::from_millis(5));
                }
                fake.editor().cancel_run().unwrap();
                cancelled.set(true);
            }
            false
        });
        assert_eq!(failure(&updates), "the run was cancelled in the Editor");
    }
}
