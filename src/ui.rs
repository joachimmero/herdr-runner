//! Drawing the pane from the app's state.

use std::path::Path;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

use crate::app::{App, Busy, BusyJob, Mark, Notice};
use crate::runner::Phase;
use crate::tree::{Row, TestNode, Tree};
use crate::unity::Verdict;

pub fn draw_no_project(frame: &mut Frame, cwd: &Path) {
    let text = vec![
        Line::from("herdr-runner".bold()),
        Line::from(""),
        Line::from(format!("No Unity project at or above {}.", cwd.display())),
        Line::from(
            "Open the pane from a directory inside one (it needs Assets/ and ProjectSettings/).",
        ),
        Line::from(""),
        Line::from("q to close".dim()),
    ];
    frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), frame.area());
}

pub fn draw(frame: &mut Frame, app: &App) {
    let detail = detail(app);
    let [header, body, footer] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(3), Constraint::Length(1)])
            .areas(frame.area());
    let (tree_area, detail_area) = match &detail {
        Some(_) => {
            let [tree, detail] =
                Layout::vertical([Constraint::Percentage(55), Constraint::Percentage(45)])
                    .areas(body);
            (tree, Some(detail))
        }
        None => (body, None),
    };
    app.tree_height.set(tree_area.height);

    frame.render_widget(Paragraph::new(header_line(app)), header);
    draw_tree(frame, app, tree_area);
    if let (Some(area), Some((title, lines))) = (detail_area, detail) {
        let block = Block::default().borders(Borders::TOP).title(title);
        frame.render_widget(
            Paragraph::new(lines)
                .block(block)
                .wrap(Wrap { trim: false })
                .scroll((app.detail_scroll, 0)),
            area,
        );
    }
    frame.render_widget(Paragraph::new(footer_line(app)), footer);
}

fn header_line(app: &App) -> Line<'static> {
    let project =
        app.project.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    let mut spans = vec![
        Span::styled("herdr-runner", Style::new().bold()),
        Span::raw(format!(" · {project} · ")),
        Span::styled(app.mode.label(), Style::new().fg(Color::Cyan)),
        Span::raw("  "),
    ];
    match &app.busy {
        Some(busy) => spans.push(Span::styled(busy_text(busy), Style::new().fg(Color::Yellow))),
        None => spans.extend(summary(app)),
    }
    Line::from(spans)
}

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

fn busy_text(busy: &Busy) -> String {
    let elapsed = busy.since.elapsed();
    let frame = SPINNER[(elapsed.as_millis() / 100) as usize % SPINNER.len()];
    let phase = match (busy.job, busy.phase) {
        (_, Phase::Checking) => "reaching the Editor".to_owned(),
        (_, Phase::Compiling) => "compiling".to_owned(),
        (BusyJob::Loading, Phase::Listing) => "listing tests".to_owned(),
        (BusyJob::Running, Phase::Listing) => "refreshing tests".to_owned(),
        (_, Phase::Waiting) => "waiting for another test run".to_owned(),
        (_, Phase::Running { of: 1, .. }) => "running".to_owned(),
        (_, Phase::Running { step, of }) => format!("running {step}/{of}"),
    };
    let cancel = if busy.job == BusyJob::Running { " · c to cancel" } else { "" };
    format!("{frame} {phase} {}s{cancel}", elapsed.as_secs())
}

/// The current mode's counts, over the tests the listing still holds.
fn summary(app: &App) -> Vec<Span<'static>> {
    let Some(tree) = &app.tree else { return Vec::new() };
    let verdicts: Vec<Verdict> =
        tree.tests().filter_map(|(_, test)| app.verdict(&test.full_name)).collect();
    let count = |verdict| verdicts.iter().filter(|&&seen| seen == verdict).count();
    let total = tree.tests().count();
    if verdicts.is_empty() {
        return vec![Span::styled(format!("{total} tests"), Style::new().dim())];
    }
    let skipped = count(Verdict::Skipped) + count(Verdict::Inconclusive);
    let mut spans = vec![
        Span::styled(format!("✓ {}", count(Verdict::Passed)), Style::new().fg(Color::Green)),
        Span::raw("  "),
        Span::styled(format!("✗ {}", count(Verdict::Failed)), Style::new().fg(Color::Red)),
    ];
    if skipped > 0 {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(format!("○ {skipped}"), Style::new().fg(Color::Yellow)));
    }
    spans.push(Span::styled(format!("  of {total}"), Style::new().dim()));
    spans
}

fn draw_tree(frame: &mut Frame, app: &App, area: Rect) {
    let Some(tree) = &app.tree else {
        let text =
            if app.busy.is_some() { "Loading tests…" } else { "No test listing. r to retry." };
        frame.render_widget(Paragraph::new(text.dim()), area);
        return;
    };
    let rows = app.rows();
    if rows.is_empty() {
        let text = if app.filter.is_empty() {
            format!("No {} tests in this project.", app.mode.label())
        } else {
            format!("No tests match '{}'.", app.filter)
        };
        frame.render_widget(Paragraph::new(text.dim()), area);
        return;
    }
    let items: Vec<ListItem> =
        rows.iter().map(|&row| ListItem::new(row_line(app, tree, row))).collect();
    let list = List::new(items).highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    // Pulled back so a list that got shorter, or a taller pane, still fills the screen.
    let offset = app.tree_offset.get().min(rows.len().saturating_sub(area.height as usize));
    let mut state = ListState::default().with_selected(Some(app.selected)).with_offset(offset);
    frame.render_stateful_widget(list, area, &mut state);
    app.tree_offset.set(state.offset());
}

/// The combined state of some tests, worst first.
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Running,
    Failed,
    Passed,
    Skipped,
    NotRun,
}

fn state_of(app: &App, tests: &[&TestNode]) -> State {
    let states: Vec<State> = tests
        .iter()
        .map(|test| match app.marks().and_then(|marks| marks.get(&test.full_name)) {
            Some(Mark::Running) => State::Running,
            Some(Mark::Done(outcome)) => match outcome.verdict {
                Verdict::Failed => State::Failed,
                Verdict::Passed => State::Passed,
                Verdict::Skipped | Verdict::Inconclusive => State::Skipped,
            },
            None => State::NotRun,
        })
        .collect();
    [State::Running, State::Failed]
        .into_iter()
        .find(|worst| states.contains(worst))
        .or_else(|| {
            // A branch reads as passed only once every non-explicit test under it has run.
            let ran = tests
                .iter()
                .zip(&states)
                .all(|(test, state)| test.explicit || *state != State::NotRun);
            let any = states.iter().any(|state| matches!(state, State::Passed | State::Skipped));
            (ran && any).then(|| {
                if states.contains(&State::Passed) { State::Passed } else { State::Skipped }
            })
        })
        .unwrap_or(State::NotRun)
}

fn glyph(app: &App, state: State) -> Span<'static> {
    match state {
        State::Running => {
            let tick = app.busy.as_ref().map_or(0, |busy| busy.since.elapsed().as_millis() / 100);
            Span::styled(SPINNER[tick as usize % SPINNER.len()], Style::new().fg(Color::Cyan))
        }
        State::Failed => Span::styled("✗", Style::new().fg(Color::Red)),
        State::Passed => Span::styled("✓", Style::new().fg(Color::Green)),
        State::Skipped => Span::styled("○", Style::new().fg(Color::Yellow)),
        State::NotRun => Span::styled("·", Style::new().dim()),
    }
}

fn row_line(app: &App, tree: &Tree, row: Row) -> Line<'static> {
    let state = glyph(app, state_of(app, &tree.tests_under(row)));
    let open = |is_open: bool| if is_open || !app.filter.is_empty() { "▾ " } else { "▸ " };
    match row {
        Row::Assembly(a) => {
            let assembly = tree.assembly(a);
            Line::from(vec![
                Span::raw(open(app.expansion.is_assembly_open(&assembly.name))),
                state,
                Span::raw(" "),
                Span::styled(assembly.name.clone(), Style::new().bold()),
            ])
        }
        Row::Fixture(a, f) => {
            let fixture = tree.fixture(a, f);
            let name = if fixture.name.is_empty() {
                "(no fixture)".to_owned()
            } else {
                fixture.name.clone()
            };
            Line::from(vec![
                Span::raw("  "),
                Span::raw(open(
                    app.expansion.is_fixture_open(&tree.assembly(a).name, &fixture.name),
                )),
                state,
                Span::raw(" "),
                Span::raw(name),
                Span::styled(format!(" ({})", fixture.tests.len()), Style::new().dim()),
            ])
        }
        Row::Test(a, f, t) => {
            let test = tree.test(a, f, t);
            let mut spans =
                vec![Span::raw("      "), state, Span::raw(" "), Span::raw(test.name.clone())];
            if let Some(Mark::Done(outcome)) =
                app.marks().and_then(|marks| marks.get(&test.full_name))
            {
                spans.push(Span::styled(format!(" {:.2}s", outcome.duration), Style::new().dim()));
            }
            if test.explicit {
                spans.push(Span::styled(" [explicit]", Style::new().fg(Color::Magenta)));
            }
            Line::from(spans)
        }
    }
}

/// What the lower half shows: a notice when there is one, else the selected test's failure.
fn detail(app: &App) -> Option<(Line<'static>, Vec<Line<'static>>)> {
    if let Some(notice) = &app.notice {
        return Some(match notice {
            Notice::Info(message) => (Line::from(" Note "), vec![Line::from(message.clone())]),
            Notice::Error(message) => (
                Line::from(" Error ".red()),
                vec![Line::from(message.clone()), hint("Esc to dismiss · r to retry")],
            ),
            Notice::CompileErrors(errors) if errors.is_empty() => (
                Line::from(" Compile errors ".red()),
                vec![
                    Line::from("The Editor has compile errors; see its Console."),
                    hint("Esc to dismiss"),
                ],
            ),
            Notice::CompileErrors(errors) => (
                Line::from(format!(" Compile errors ({}) ", errors.len()).red()),
                errors
                    .iter()
                    .map(|error| Line::from(error.clone()))
                    .chain([hint("Esc to dismiss")])
                    .collect(),
            ),
        });
    }
    let tree = app.tree.as_ref()?;
    let Row::Test(a, f, t) = app.selected_row()? else { return None };
    let test = tree.test(a, f, t);
    let Some(Mark::Done(outcome)) = app.marks()?.get(&test.full_name) else { return None };
    let message = outcome.message.as_deref().filter(|message| !message.trim().is_empty());
    let trace = outcome.stack_trace.as_deref().filter(|trace| !trace.trim().is_empty());
    if message.is_none() && trace.is_none() {
        return None;
    }
    let title = Line::from(format!(" {} ", test.full_name));
    let lines = message
        .into_iter()
        .flat_map(str::lines)
        .map(|line| Line::from(line.to_owned()))
        .chain(trace.map(|_| Line::from("")))
        .chain(trace.into_iter().flat_map(str::lines).map(|line| Line::from(line.to_owned()).dim()))
        .collect();
    Some((title, lines))
}

fn hint(text: &'static str) -> Line<'static> {
    Line::from(text.dim())
}

fn footer_line(app: &App) -> Line<'static> {
    if app.editing_filter {
        return Line::from(vec![
            Span::styled("/", Style::new().fg(Color::Cyan)),
            Span::raw(app.filter.clone()),
            Span::raw("▏"),
        ]);
    }
    let keys = "⏎ run · a all · f failures · m mode · c cancel · / filter · r reload · q quit";
    let mut spans = vec![Span::styled(keys, Style::new().dim())];
    if !app.filter.is_empty() {
        spans.insert(0, Span::styled(format!("/{}  ", app.filter), Style::new().fg(Color::Cyan)));
    }
    Line::from(spans)
}
