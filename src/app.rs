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

