//! The `run` verb: hand a run to the workspace's runner pane, opening one if needed, so it shows
//! there, then wait for its outcome and report it.

use std::thread;
use std::time::{Duration, Instant};

use crate::herdr::var;
use crate::mailbox::{Mailbox, Request, Response};
use crate::unity::{Mode, TestOutcome, Verdict};
use crate::{actions, project};

pub const USAGE: &str =
    "herdr-runner run [<test, fixture, or assembly name>] [--mode edit|play] [--timeout <seconds>]";

const PASSED: u8 = 0;
const PROBLEM: u8 = 1;
const BAD_USAGE: u8 = 2;
const FAILED: u8 = 5;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);
/// A pane takes a request on its first tick, so this only runs out when no pane is reading.
const PICKUP: Duration = Duration::from_secs(20);
const LIVENESS: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(100);

struct Args {
    name: Option<String>,
    mode: Mode,
    timeout: Duration,
}

/// Run the verb with `args` and return the process exit code.
pub fn run(args: &[String]) -> u8 {
    let args = match parse(args) {
        Ok(args) => args,
        Err(why) => {
            eprintln!("herdr-runner: {why}\nusage: {USAGE}");
            return BAD_USAGE;
        }
    };
    match post_and_wait(&args) {
        Ok(response) => report(&response, args.name.as_deref()),
        Err(why) => {
            eprintln!("herdr-runner: {why}");
            PROBLEM
        }
    }
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut parsed = Args { name: None, mode: Mode::EditMode, timeout: DEFAULT_TIMEOUT };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--mode" => {
                parsed.mode = match args.next().map(String::as_str) {
                    Some("edit") => Mode::EditMode,
                    Some("play") => Mode::PlayMode,
                    other => {
                        return Err(format!(
                            "--mode takes edit or play, not {}",
                            other.unwrap_or("nothing")
                        ));
                    }
                }
            }
            "--timeout" => {
                parsed.timeout = args
                    .next()
                    .and_then(|seconds| seconds.parse().ok())
                    .map(Duration::from_secs)
                    .ok_or("--timeout takes a whole number of seconds")?;
            }
            flag if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
            name if parsed.name.is_none() => parsed.name = Some(name.to_owned()),
            extra => return Err(format!("one name at most, and {extra} is a second")),
        }
    }
    Ok(parsed)
}

fn post_and_wait(args: &Args) -> Result<Response, String> {
    let cwd = std::env::current_dir().map_err(|error| format!("no working directory: {error}"))?;
    let project = project::find_root(&cwd)
        .ok_or_else(|| format!("no Unity project at or above {}", cwd.display()))?;
    let ws = var("HERDR_WORKSPACE_ID").ok_or("not inside herdr: HERDR_WORKSPACE_ID is unset")?;
    let beside = var("HERDR_PANE_ID").ok_or("not inside herdr: HERDR_PANE_ID is unset")?;
    let pane = actions::find_or_open(&ws, &project, &beside)?;

    let mailbox = Mailbox::for_pane(&pane)?;
    let id = mailbox.send(&Request { project, mode: args.mode, name: args.name.clone() })?;
    let started = Instant::now();
    let mut checked = Instant::now();
    loop {
        if let Some(response) = mailbox.response(&id) {
            mailbox.clear(&id);
            return Ok(response);
        }
        if started.elapsed() > args.timeout {
            // The pane cancels the run and cleans up after it.
            mailbox.withdraw(&id);
            return Err(format!(
                "no outcome within {}s, so the pane was asked to cancel the run",
                args.timeout.as_secs()
            ));
        }
        if started.elapsed() > PICKUP && !mailbox.is_taken(&id) {
            mailbox.clear(&id);
            return Err(format!(
                "pane {pane} did not pick up the run within {}s (is it still loading?)",
                PICKUP.as_secs()
            ));
        }
        if checked.elapsed() > LIVENESS {
            checked = Instant::now();
            // A pane that answered and then closed has still answered.
            if !actions::runs_runner(&pane) && mailbox.response(&id).is_none() {
                mailbox.clear(&id);
                return Err(format!("pane {pane} closed before the run finished"));
            }
        }
        thread::sleep(POLL);
    }
}

fn report(response: &Response, name: Option<&str>) -> u8 {
    match response {
        Response::Completed { results } => report_results(results),
        Response::CompileFailed { errors } => {
            println!("Compile errors ({}); nothing ran.", errors.len());
            for error in errors {
                println!("  {error}");
            }
            PROBLEM
        }
        Response::UnknownName { suggestions } => {
            eprintln!(
                "herdr-runner: no test, fixture, or assembly is named exactly '{}'",
                name.unwrap_or_default()
            );
            if !suggestions.is_empty() {
                eprintln!("Did you mean:");
                for suggestion in suggestions {
                    eprintln!("  {suggestion}");
                }
            }
            PROBLEM
        }
        Response::Cancelled => {
            println!("The run was cancelled in the pane.");
            PROBLEM
        }
        Response::Failed { message } => {
            eprintln!("herdr-runner: {message}");
            PROBLEM
        }
    }
}

fn report_results(results: &[TestOutcome]) -> u8 {
    let count = |verdict| results.iter().filter(|outcome| outcome.verdict == verdict).count();
    let failed: Vec<&TestOutcome> =
        results.iter().filter(|outcome| outcome.verdict == Verdict::Failed).collect();

    println!(
        "Total {} | Passed {} | Failed {} | Skipped {}",
        results.len(),
        count(Verdict::Passed),
        failed.len(),
        count(Verdict::Skipped)
    );
    for outcome in &failed {
        println!("\nFAILED {}", outcome.full_name);
        for text in [&outcome.message, &outcome.stack_trace].into_iter().flatten() {
            for line in text.trim().lines() {
                println!("  {line}");
            }
        }
    }
    match (results.is_empty(), failed.is_empty()) {
        (true, _) => {
            println!("\nNo tests ran.");
            PASSED
        }
        (false, true) => {
            println!("\nAll tests passed.");
            PASSED
        }
        (false, false) => FAILED,
    }
}
