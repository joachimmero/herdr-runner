//! The pane: its state, its keys, and the loop that ties them to the worker.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::runner::{self, Ending, Job, Phase, Update};
use crate::tree::{Expansion, Row, Tree};
use crate::unity::{Editor, Mode, RunRequest, TestOutcome, Verdict};
use crate::{project, ui};

/// A test's state in the pane, which outlives the listing it came from.
pub enum Mark {
    Running,
    Done(TestOutcome),
}

pub enum Notice {
    Info(String),
    Error(String),
    CompileErrors(Vec<String>),
}

pub struct Busy {
    pub job: BusyJob,
    pub phase: Phase,
    pub since: Instant,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BusyJob {
    Loading,
    Running,
}

pub struct App {
    pub project: PathBuf,
    pub mode: Mode,
    /// `None` until the current mode's first listing arrives.
    pub tree: Option<Tree>,
    pub marks: HashMap<Mode, HashMap<String, Mark>>,
    pub expansion: Expansion,
    pub selected: usize,
    pub filter: String,
    pub editing_filter: bool,
    pub busy: Option<Busy>,
    pub notice: Option<Notice>,
    pub detail_scroll: u16,
    editor: Editor,
    jobs: Sender<Job>,
    updates: Receiver<Update>,
    cancel: Arc<AtomicBool>,
    quit: bool,
}

pub fn run() -> u8 {
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut terminal = ratatui::init();
    let outcome = match project::find_root(&cwd) {
        Some(project) => App::new(project.clone(), Editor::new(project)).run(&mut terminal),
        None => show_no_project(&mut terminal, &cwd),
    };
    ratatui::restore();
    match outcome {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("herdr-runner: {error}");
            1
        }
    }
}

fn show_no_project(terminal: &mut DefaultTerminal, cwd: &Path) -> io::Result<()> {
    loop {
        terminal.draw(|frame| ui::draw_no_project(frame, cwd))?;
        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
            && is_quit(&key)
        {
            return Ok(());
        }
    }
}

fn is_quit(key: &KeyEvent) -> bool {
    key.code == KeyCode::Char('q')
        || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
}

const TICK: Duration = Duration::from_millis(100);

impl App {
    fn new(project: PathBuf, editor: Editor) -> Self {
        let (jobs, job_rx) = mpsc::channel();
        let (update_tx, updates) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        runner::spawn(editor.clone(), job_rx, update_tx, Arc::clone(&cancel));
        let mut app = Self {
            project,
            editor,
            mode: Mode::EditMode,
            tree: None,
            marks: HashMap::new(),
            expansion: Expansion::default(),
            selected: 0,
            filter: String::new(),
            editing_filter: false,
            busy: None,
            notice: None,
            detail_scroll: 0,
            jobs,
            updates,
            cancel,
            quit: false,
        };
        app.load();
        app
    }

    fn run(mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        while !self.quit {
            terminal.draw(|frame| ui::draw(frame, &self))?;
            if event::poll(TICK)?
                && let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                self.on_key(key);
            }
            while let Ok(update) = self.updates.try_recv() {
                self.on_update(update);
            }
        }
        self.shut_down();
        Ok(())
    }

    /// A run left going would keep the Editor busy after the pane is gone.
    fn shut_down(&self) {
        if self.busy.as_ref().is_some_and(|busy| matches!(busy.phase, Phase::Running { .. })) {
            let _ = self.editor.cancel_run();
        }
    }

    pub fn rows(&self) -> Vec<Row> {
        self.tree.as_ref().map(|tree| tree.rows(&self.expansion, &self.filter)).unwrap_or_default()
    }

    pub fn marks(&self) -> Option<&HashMap<String, Mark>> {
        self.marks.get(&self.mode)
    }

    pub fn selected_row(&self) -> Option<Row> {
        self.rows().get(self.selected).copied()
    }

    fn on_key(&mut self, key: KeyEvent) {
        if self.editing_filter {
            self.on_filter_key(key);
            return;
        }
        if is_quit(&key) {
            self.quit = true;
            return;
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.move_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_by(-1),
            KeyCode::Char('g') | KeyCode::Home => self.select(0),
            KeyCode::Char('G') | KeyCode::End => self.select(usize::MAX),
            KeyCode::Char('l') | KeyCode::Right => self.set_open(true),
            KeyCode::Char('h') | KeyCode::Left => self.set_open(false),
            KeyCode::Char(' ') => self.toggle_open(),
            KeyCode::Char('J') => self.detail_scroll = self.detail_scroll.saturating_add(1),
            KeyCode::Char('K') => self.detail_scroll = self.detail_scroll.saturating_sub(1),
            KeyCode::Enter => self.run_selection(),
            KeyCode::Char('a') => self.start_run(vec![RunRequest::all()]),
            KeyCode::Char('f') => self.rerun_failures(),
            KeyCode::Char('m') => self.switch_mode(),
            KeyCode::Char('r') => self.load(),
            KeyCode::Char('c') => self.cancel(),
            KeyCode::Char('/') => self.editing_filter = true,
            KeyCode::Esc if self.notice.is_some() => self.notice = None,
            KeyCode::Esc => self.set_filter(String::new()),
            _ => {}
        }
    }

    fn on_filter_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => self.editing_filter = false,
            KeyCode::Esc => {
                self.editing_filter = false;
                self.set_filter(String::new());
            }
            KeyCode::Backspace if self.filter.is_empty() => self.editing_filter = false,
            KeyCode::Backspace => {
                let mut filter = self.filter.clone();
                filter.pop();
                self.set_filter(filter);
            }
            KeyCode::Char(ch) => self.set_filter(format!("{}{ch}", self.filter)),
            _ => {}
        }
    }

    fn set_filter(&mut self, filter: String) {
        self.filter = filter;
        self.select(0);
    }

    fn move_by(&mut self, delta: isize) {
        self.select(self.selected.saturating_add_signed(delta));
    }

    fn select(&mut self, index: usize) {
        let last = self.rows().len().saturating_sub(1);
        self.selected = index.min(last);
        self.detail_scroll = 0;
    }

    fn set_open(&mut self, open: bool) {
        let (Some(tree), Some(row)) = (&self.tree, self.selected_row()) else { return };
        match row {
            Row::Assembly(a) => self.expansion.set_assembly(&tree.assembly(a).name, open),
            Row::Fixture(a, f) => {
                self.expansion.set_fixture(&tree.assembly(a).name, &tree.fixture(a, f).name, open)
            }
            // Closing from a test closes its fixture and lands on it.
            Row::Test(a, f, _) if !open => {
                self.expansion.set_fixture(&tree.assembly(a).name, &tree.fixture(a, f).name, false);
                let fixture = self.rows().iter().position(|row| *row == Row::Fixture(a, f));
                self.select(fixture.unwrap_or(0));
            }
            Row::Test(..) => {}
        }
        self.select(self.selected);
    }

    fn toggle_open(&mut self) {
        let (Some(tree), Some(row)) = (&self.tree, self.selected_row()) else { return };
        let open = match row {
            Row::Assembly(a) => self.expansion.is_assembly_open(&tree.assembly(a).name),
            Row::Fixture(a, f) => {
                self.expansion.is_fixture_open(&tree.assembly(a).name, &tree.fixture(a, f).name)
            }
            Row::Test(..) => return,
        };
        self.set_open(!open);
    }

    fn run_selection(&mut self) {
        let (Some(tree), Some(row)) = (&self.tree, self.selected_row()) else { return };
        let requests = match row {
            Row::Assembly(a) => vec![RunRequest::assembly(&tree.assembly(a).name)],
            // A test outside any fixture has no prefix to share, so each goes by name.
            Row::Fixture(a, f) if tree.fixture(a, f).name.is_empty() => tree
                .tests_under(row)
                .into_iter()
                .filter(|test| !test.explicit)
                .map(|test| RunRequest::test(&test.full_name, false))
                .collect(),
            Row::Fixture(a, f) => vec![RunRequest::fixture(&tree.fixture(a, f).name)],
            Row::Test(a, f, t) => {
                let test = tree.test(a, f, t);
                vec![RunRequest::test(&test.full_name, test.explicit)]
            }
        };
        self.start_run(requests);
    }

    fn rerun_failures(&mut self) {
        let Some(tree) = &self.tree else { return };
        let requests: Vec<RunRequest> = tree
            .tests()
            .filter(|(_, test)| self.verdict(&test.full_name) == Some(Verdict::Failed))
            .map(|(_, test)| RunRequest::test(&test.full_name, test.explicit))
            .collect();
        if requests.is_empty() {
            self.notice =
                Some(Notice::Info(format!("No {} failures to re-run.", self.mode.label())));
            return;
        }
        self.start_run(requests);
    }

    pub fn verdict(&self, full_name: &str) -> Option<Verdict> {
        match self.marks()?.get(full_name)? {
            Mark::Done(outcome) => Some(outcome.verdict),
            Mark::Running => None,
        }
    }

    fn start_run(&mut self, requests: Vec<RunRequest>) {
        let Some(tree) = &self.tree else { return };
        if self.busy.is_some() || requests.is_empty() {
            return;
        }
        let expected: Vec<String> = tree
            .tests()
            .filter(|(assembly, test)| {
                requests
                    .iter()
                    .any(|request| request.covers(assembly, &test.full_name, test.explicit))
            })
            .map(|(_, test)| test.full_name.clone())
            .collect();

        let marks = self.marks.entry(self.mode).or_default();
        marks.extend(expected.into_iter().map(|name| (name, Mark::Running)));
        self.notice = None;
        self.cancel.store(false, Ordering::Relaxed);
        self.busy =
            Some(Busy { job: BusyJob::Running, phase: Phase::Checking, since: Instant::now() });
        let _ = self.jobs.send(Job::Run { mode: self.mode, requests });
    }

    fn load(&mut self) {
        if self.busy.is_some() {
            return;
        }
        self.busy =
            Some(Busy { job: BusyJob::Loading, phase: Phase::Checking, since: Instant::now() });
        let _ = self.jobs.send(Job::Load(self.mode));
    }

    fn switch_mode(&mut self) {
        if self.busy.is_some() {
            self.notice =
                Some(Notice::Info("Wait for the current job, or cancel it with c.".into()));
            return;
        }
        self.mode = self.mode.other();
        self.tree = None;
        self.notice = None;
        self.select(0);
        self.load();
    }

    fn cancel(&mut self) {
        if self.busy.as_ref().is_some_and(|busy| busy.job == BusyJob::Running) {
            self.cancel.store(true, Ordering::Relaxed);
        }
    }

    fn on_update(&mut self, update: Update) {
        match update {
            Update::Phase(phase) => {
                if let Some(busy) = &mut self.busy {
                    busy.phase = phase;
                }
            }
            Update::Listed(mode, items) if mode == self.mode => {
                self.tree = Some(Tree::build(&items));
                self.select(self.selected);
            }
            Update::Listed(..) => {}
            Update::CompileFailed(errors) => self.notice = Some(Notice::CompileErrors(errors)),
            Update::Results(mode, outcomes) => self.record(mode, outcomes),
            Update::Finished(ending) => self.finish(ending),
        }
    }

    /// Store a run's outcomes and open every fixture holding a failure, so it is one move away.
    fn record(&mut self, mode: Mode, outcomes: Vec<TestOutcome>) {
        let failing: Vec<(String, String)> = self
            .tree
            .iter()
            .flat_map(|tree| {
                tree.assemblies.iter().flat_map(|assembly| {
                    assembly
                        .fixtures
                        .iter()
                        .filter(|fixture| {
                            fixture.tests.iter().any(|test| {
                                outcomes.iter().any(|outcome| {
                                    outcome.verdict == Verdict::Failed
                                        && outcome.full_name == test.full_name
                                })
                            })
                        })
                        .map(|fixture| (assembly.name.clone(), fixture.name.clone()))
                })
            })
            .collect();

        let marks = self.marks.entry(mode).or_default();
        marks.extend(
            outcomes.into_iter().map(|outcome| (outcome.full_name.clone(), Mark::Done(outcome))),
        );
        if mode == self.mode {
            for (assembly, fixture) in failing {
                self.expansion.set_fixture(&assembly, &fixture, true);
            }
        }
    }

    fn finish(&mut self, ending: Ending) {
        self.busy = None;
        // A test marked running that no result came back for didn't run after all.
        for marks in self.marks.values_mut() {
            marks.retain(|_, mark| matches!(mark, Mark::Done(_)));
        }
        match ending {
            Ending::Done | Ending::CompileFailed => {}
            Ending::Cancelled => self.notice = Some(Notice::Info("Run cancelled.".into())),
            Ending::Failed(message) => self.notice = Some(Notice::Error(message)),
        }
        self.select(self.selected);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use serde_json::{Value, json};

    use super::*;
    use crate::fake::{FakeEditor, suite};

    fn app(fake: &FakeEditor) -> App {
        let mut app = App::new(fake.project(), fake.editor());
        settle(&mut app);
        app
    }

    /// Apply updates until the job in flight is over.
    fn settle(app: &mut App) {
        while app.busy.is_some() {
            let update =
                app.updates.recv_timeout(Duration::from_secs(20)).expect("job never ended");
            app.on_update(update);
        }
    }

    /// Apply updates until the run reaches its `Running` phase.
    fn until_running(app: &mut App) {
        while !app.busy.as_ref().is_some_and(|busy| matches!(busy.phase, Phase::Running { .. })) {
            let update = app.updates.recv_timeout(Duration::from_secs(20)).expect("never ran");
            app.on_update(update);
        }
    }

    fn press(app: &mut App, keys: &str) {
        for ch in keys.chars() {
            let code = match ch {
                '\n' => KeyCode::Enter,
                '\u{1b}' => KeyCode::Esc,
                '\u{8}' => KeyCode::Backspace,
                ch => KeyCode::Char(ch),
            };
            app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
        }
    }

    fn select(app: &mut App, wanted: impl Fn(&Tree, Row) -> bool) {
        let tree = app.tree.as_ref().unwrap();
        app.selected =
            app.rows().into_iter().position(|row| wanted(tree, row)).expect("no such row");
    }

    fn select_fixture(app: &mut App, name: &str) {
        select(
            app,
            |tree, row| matches!(row, Row::Fixture(a, f) if tree.fixture(a, f).name == name),
        );
    }

    fn select_test(app: &mut App, full_name: &str) {
        let (assembly, fixture) = {
            let tree = app.tree.as_ref().unwrap();
            let (a, f) = tree
                .assemblies
                .iter()
                .enumerate()
                .flat_map(|(a, assembly)| (0..assembly.fixtures.len()).map(move |f| (a, f)))
                .find(|&(a, f)| {
                    tree.fixture(a, f).tests.iter().any(|test| test.full_name == full_name)
                })
                .unwrap();
            (tree.assembly(a).name.clone(), tree.fixture(a, f).name.clone())
        };
        app.expansion.set_fixture(&assembly, &fixture, true);
        select(
            app,
            |tree, row| matches!(row, Row::Test(a, f, t) if tree.test(a, f, t).full_name == full_name),
        );
    }

    fn is_running(app: &App, full_name: &str) -> bool {
        matches!(app.marks().and_then(|marks| marks.get(full_name)), Some(Mark::Running))
    }

    fn screen(app: &App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| ui::draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| (0..buffer.area.width).map(|x| buffer[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn opening_lists_the_tree_with_fixtures_closed() {
        let fake = FakeEditor::new(json!({"tests": suite()}));
        let app = app(&fake);
        let tree = app.tree.as_ref().unwrap();
        let assemblies: Vec<&str> = tree.assemblies.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(assemblies, ["Game.Tests", "Words.Tests"]);
        assert!(app.rows().iter().all(|row| !matches!(row, Row::Test(..))));
        assert!(app.notice.is_none());
        assert!(fake.runs().is_empty());
    }

    #[test]
    fn running_a_fixture_marks_exactly_what_unity_will_run() {
        let fake = FakeEditor::new(json!({"tests": suite(), "run_polls": 3}));
        let mut app = app(&fake);
        select_fixture(&mut app, "Game.BoardTests");
        press(&mut app, "\n");

        assert!(is_running(&app, "Game.BoardTests.PlacesTile"));
        assert!(is_running(&app, "Game.BoardTests.RejectsOverlap"));
        assert!(!is_running(&app, "Game.BoardTestsExtra.Scores(1.5)"));
        settle(&mut app);

        assert_eq!(app.verdict("Game.BoardTests.PlacesTile"), Some(Verdict::Passed));
        assert_eq!(app.verdict("Game.BoardTests.RejectsOverlap"), Some(Verdict::Failed));
        assert_eq!(app.verdict("Game.BoardTestsExtra.Scores(1.5)"), None);
        // The fixture holding the failure opens, so the failure is one move away.
        assert!(app.expansion.is_fixture_open("Game.Tests", "Game.BoardTests"));
        assert!(app.notice.is_none());
    }

    #[test]
    fn a_selected_failure_shows_its_message_and_trace() {
        let fake = FakeEditor::new(json!({"tests": suite()}));
        let mut app = app(&fake);
        press(&mut app, "a");
        settle(&mut app);
        select_test(&mut app, "Game.BoardTests.RejectsOverlap");

        let screen = screen(&app);
        assert!(screen.contains("Game.BoardTests.RejectsOverlap"), "{screen}");
        assert!(screen.contains("Expected: 1"), "{screen}");
        assert!(screen.contains("BoardTests.cs:42"), "{screen}");
        assert!(screen.contains("✓ 3  ✗ 1  of 5"), "{screen}");
    }

    #[test]
    fn rerunning_failures_runs_each_failed_test_by_name() {
        let fake = FakeEditor::new(json!({"tests": suite()}));
        let mut app = app(&fake);
        press(&mut app, "a");
        settle(&mut app);
        press(&mut app, "f");
        settle(&mut app);

        let filters: Vec<Value> = fake.runs().iter().map(|run| run["filter"].clone()).collect();
        assert_eq!(filters, [Value::Null, json!("Game.BoardTests.RejectsOverlap")]);
        assert_eq!(app.verdict("Game.BoardTests.RejectsOverlap"), Some(Verdict::Failed));
        // Results from the first run survive the second.
        assert_eq!(app.verdict("Words.DictionaryTests.Loads"), Some(Verdict::Passed));
    }

    #[test]
    fn rerunning_with_no_failures_says_so() {
        let fake = FakeEditor::new(json!({"tests": suite()}));
        let mut app = app(&fake);
        press(&mut app, "f");
        assert!(
            matches!(&app.notice, Some(Notice::Info(message)) if message.contains("No EditMode failures"))
        );
        assert!(app.busy.is_none());
        assert!(fake.runs().is_empty());
    }

    #[test]
    fn running_an_explicit_test_by_selection_includes_it() {
        let fake = FakeEditor::new(json!({"tests": suite()}));
        let mut app = app(&fake);
        select_test(&mut app, "Game.Sim.RunsForever");
        press(&mut app, "\n");
        settle(&mut app);
        assert_eq!(fake.runs()[0]["include_explicit"], "true");
        assert_eq!(app.verdict("Game.Sim.RunsForever"), Some(Verdict::Passed));
    }

    #[test]
    fn a_fixture_with_only_an_explicit_test_never_counts_it_as_expected() {
        let fake = FakeEditor::new(json!({"tests": suite(), "run_polls": 100_000}));
        let mut app = app(&fake);
        press(&mut app, "a");
        assert!(!is_running(&app, "Game.Sim.RunsForever"));
        assert!(is_running(&app, "Words.DictionaryTests.Loads"));
        press(&mut app, "c");
        settle(&mut app);
    }

    #[test]
    fn a_compile_failure_shows_the_errors_and_clears_the_running_marks() {
        let errors = ["Assets/Board.cs(3,1): error CS1002: ; expected"];
        let fake = FakeEditor::new(
            json!({"tests": suite(), "compile": {"start": "failed", "errors": errors}}),
        );
        let mut app = app(&fake);
        press(&mut app, "a");
        settle(&mut app);

        assert!(matches!(&app.notice, Some(Notice::CompileErrors(shown)) if shown == &errors));
        assert!(!is_running(&app, "Game.BoardTests.PlacesTile"));
        let screen = screen(&app);
        assert!(screen.contains("Compile errors (1)"), "{screen}");
        assert!(screen.contains("error CS1002"), "{screen}");

        press(&mut app, "\u{1b}");
        assert!(app.notice.is_none());
    }

    #[test]
    fn cancelling_stops_the_run_and_clears_the_running_marks() {
        let fake = FakeEditor::new(json!({"tests": suite(), "run_polls": 100_000}));
        let mut app = app(&fake);
        press(&mut app, "a");
        until_running(&mut app);
        press(&mut app, "c");
        settle(&mut app);

        assert!(matches!(&app.notice, Some(Notice::Info(message)) if message == "Run cancelled."));
        assert!(app.marks().unwrap().is_empty());
        assert!(fake.commands().contains(&"cancel_tests".to_owned()));
    }

    #[test]
    fn a_second_run_is_ignored_while_one_is_going() {
        let fake = FakeEditor::new(json!({"tests": suite(), "run_polls": 5}));
        let mut app = app(&fake);
        press(&mut app, "a");
        press(&mut app, "af\n");
        settle(&mut app);
        assert_eq!(fake.runs().len(), 1);
    }

    #[test]
    fn switching_mode_lists_the_other_suite_and_keeps_each_modes_results() {
        let fake = FakeEditor::new(json!({"tests": suite()}));
        let mut app = app(&fake);
        press(&mut app, "a");
        settle(&mut app);
        press(&mut app, "m");
        settle(&mut app);

        assert_eq!(app.mode, Mode::PlayMode);
        assert_eq!(app.tree.as_ref().unwrap().tests().count(), 1);
        assert_eq!(fake.calls().last().unwrap()["args"]["mode"], "playmode");
        press(&mut app, "a");
        settle(&mut app);
        assert_eq!(fake.runs().last().unwrap()["mode"], "playmode");
        assert_eq!(app.verdict("Game.PlayTests.Boots"), Some(Verdict::Passed));

        press(&mut app, "m");
        settle(&mut app);
        assert_eq!(app.verdict("Game.BoardTests.RejectsOverlap"), Some(Verdict::Failed));
    }

    #[test]
    fn switching_mode_waits_for_the_job_in_flight() {
        let fake = FakeEditor::new(json!({"tests": suite(), "run_polls": 5}));
        let mut app = app(&fake);
        press(&mut app, "am");
        assert_eq!(app.mode, Mode::EditMode);
        assert!(matches!(&app.notice, Some(Notice::Info(_))));
        settle(&mut app);
    }

    #[test]
    fn the_filter_narrows_the_tree_and_enter_runs_what_is_selected() {
        let fake = FakeEditor::new(json!({"tests": suite()}));
        let mut app = app(&fake);
        press(&mut app, "/DICT\n");
        assert!(!app.editing_filter);
        assert_eq!(app.rows(), [Row::Assembly(1), Row::Fixture(1, 0), Row::Test(1, 0, 0)]);
        press(&mut app, "jj\n");
        settle(&mut app);
        assert_eq!(fake.runs()[0]["filter"], "Words.DictionaryTests.Loads");

        press(&mut app, "\u{1b}");
        assert!(app.filter.is_empty());
        assert!(app.rows().len() > 3);
    }

    #[test]
    fn typing_in_the_filter_does_not_trigger_keys() {
        let fake = FakeEditor::new(json!({"tests": suite()}));
        let mut app = app(&fake);
        press(&mut app, "/aqmfc");
        assert_eq!(app.filter, "aqmfc");
        assert!(!app.quit);
        assert_eq!(app.mode, Mode::EditMode);
        assert!(app.busy.is_none());
    }

    #[test]
    fn backspace_on_an_empty_filter_leaves_it() {
        let fake = FakeEditor::new(json!({"tests": suite()}));
        let mut app = app(&fake);
        press(&mut app, "/ab\u{8}\u{8}");
        assert!(app.editing_filter);
        assert!(app.filter.is_empty());
        press(&mut app, "\u{8}");
        assert!(!app.editing_filter);
        press(&mut app, "q");
        assert!(app.quit);
    }

    #[test]
    fn closing_from_a_test_lands_on_its_fixture() {
        let fake = FakeEditor::new(json!({"tests": suite()}));
        let mut app = app(&fake);
        select_test(&mut app, "Game.BoardTests.RejectsOverlap");
        press(&mut app, "h");
        assert!(!app.expansion.is_fixture_open("Game.Tests", "Game.BoardTests"));
        let tree = app.tree.as_ref().unwrap();
        let row = app.selected_row().unwrap();
        assert!(matches!(row, Row::Fixture(a, f) if tree.fixture(a, f).name == "Game.BoardTests"));
    }

    #[test]
    fn an_unreachable_editor_shows_an_error_and_retries_on_r() {
        let fake = FakeEditor::new(json!({"tests": suite(), "flaky": {"editor_status": 1}}));
        let mut app = app(&fake);
        assert!(
            matches!(&app.notice, Some(Notice::Error(message)) if message.contains("No Unity Editor"))
        );
        assert!(app.tree.is_none());
        assert!(screen(&app).contains("No test listing. r to retry."));

        press(&mut app, "r");
        settle(&mut app);
        assert!(app.tree.is_some());
    }

    #[test]
    fn quitting_mid_run_cancels_it_in_the_editor() {
        let fake = FakeEditor::new(json!({"tests": suite(), "run_polls": 100_000}));
        let mut app = app(&fake);
        press(&mut app, "a");
        until_running(&mut app);
        while fake.runs().is_empty() {
            std::thread::sleep(Duration::from_millis(5));
        }
        press(&mut app, "q");
        assert!(app.quit);
        app.shut_down();
        assert_eq!(fake.commands().last().unwrap(), "cancel_tests");
    }

    #[test]
    fn quitting_while_idle_leaves_the_editor_alone() {
        let fake = FakeEditor::new(json!({"tests": suite()}));
        let mut app = app(&fake);
        press(&mut app, "q");
        app.shut_down();
        assert!(!fake.commands().contains(&"cancel_tests".to_owned()));
    }
}
