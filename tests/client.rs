//! The `run` verb, run as an agent's shell runs it, against `tests/fakes/herdr.py` and a stand-in
//! for the pane that answers the mailbox the way the pane does.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use common::{Workspace, option, runner, shell, stderr, stdout};
use serde_json::{Value, json};

/// Start `run args` from the project's script folder, with the env an agent's shell in pane
/// `w1:p1` has: no plugin variables, so the state dir comes from `HOME`.
fn start(ws: &Workspace, args: &[&str]) -> Child {
    Command::new(env!("CARGO_BIN_EXE_herdr-runner"))
        .arg("run")
        .args(args)
        .current_dir(ws.project().join("Assets/Scripts"))
        .env("FAKE_HERDR_DIR", &ws.dir)
        .env("HERDR_BIN_PATH", ws.herdr())
        .env("HERDR_WORKSPACE_ID", "w1")
        .env("HERDR_PANE_ID", "w1:p1")
        .env("HOME", &ws.dir)
        .env_remove("HERDR_PLUGIN_ID")
        .env_remove("HERDR_PLUGIN_STATE_DIR")
        .env_remove("XDG_STATE_HOME")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn mailbox(ws: &Workspace, pane: &str) -> PathBuf {
    ws.dir
        .join(".local/state/herdr/plugins/joachimmero.herdr-runner/mailbox")
        .join(pane.replace(':', "_"))
}

/// Take the first request posted to `dir` as the pane does, returning its id and body.
fn take(dir: &Path) -> (String, Value) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(Instant::now() < deadline, "no request reached {}", dir.display());
        let posted = std::fs::read_dir(dir).into_iter().flatten().flatten().find_map(|entry| {
            entry.file_name().to_str()?.strip_suffix(".request").map(str::to_owned)
        });
        if let Some(id) = posted {
            let request = dir.join(format!("{id}.request"));
            let body = serde_json::from_slice(&std::fs::read(&request).unwrap()).unwrap();
            std::fs::rename(&request, dir.join(format!("{id}.taken"))).unwrap();
            return (id, body);
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn respond(dir: &Path, id: &str, response: Value) {
    std::fs::write(dir.join(format!("{id}.response")), response.to_string()).unwrap();
}

/// Answer the first request with `response` and return the client's output and the request.
fn serve(ws: &Workspace, pane: &str, args: &[&str], response: Value) -> (Output, Value) {
    let child = start(ws, args);
    let dir = mailbox(ws, pane);
    let (id, request) = take(&dir);
    respond(&dir, &id, response);
    (child.wait_with_output().unwrap(), request)
}

fn one_failure() -> Value {
    json!({"kind": "completed", "results": [
        {"FullName": "Game.BoardTests.PlacesTile", "Status": "Passed"},
        {"FullName": "Game.BoardTests.RejectsOverlap", "Status": "Failed",
         "Message": "Expected: 1\n  But was: 2",
         "StackTrace": "at Game.BoardTests.RejectsOverlap () in BoardTests.cs:42\nat Next.Frame ()"},
    ]})
}

#[test]
fn opens_a_pane_beside_the_agent_without_focus_and_reports_each_failure_in_full() {
    let ws = Workspace::new(|dir| json!([shell("w1:p1", &dir.join("elsewhere"))]));
    let (output, request) = serve(&ws, "w1:p11", &["Game.BoardTests"], one_failure());

    assert_eq!(output.status.code(), Some(5), "{}", stderr(&output));
    let out = stdout(&output);
    assert!(out.starts_with("Total 2 | Passed 1 | Failed 1 | Skipped 0\n"), "{out}");
    assert!(out.contains("\nFAILED Game.BoardTests.RejectsOverlap\n"), "{out}");
    assert!(out.contains("  Expected: 1\n    But was: 2\n"), "{out}");
    assert!(out.contains("  at Next.Frame ()"), "{out}");

    assert_eq!(request["name"], "Game.BoardTests");
    assert_eq!(request["mode"], "EditMode");
    let opens = ws.calls_to(&["plugin", "pane", "open"]);
    assert_eq!(opens.len(), 1);
    assert_eq!(option(&opens[0], "--target-pane"), Some("w1:p1"));
    // The project the agent stands in, not its shell's nested folder or the pane's cwd.
    let cwd = Path::new(option(&opens[0], "--cwd").unwrap()).canonicalize().unwrap();
    assert_eq!(cwd, ws.project().canonicalize().unwrap());
    assert!(!opens[0].contains(&"--focus".to_owned()));
    // The client clears its request once it has the answer.
    assert_eq!(std::fs::read_dir(mailbox(&ws, "w1:p11")).unwrap().count(), 0);
}

#[test]
fn reuses_an_open_pane_and_passes_with_every_test_passing() {
    let ws = Workspace::new(|dir| {
        let project = dir.join("project");
        json!([shell("w1:p1", &project), runner("w1:p2", &project, &["/p/bin/herdr-runner"])])
    });
    let passed = json!({"kind": "completed", "results": [
        {"FullName": "Words.DictionaryTests.Loads", "Status": "Passed"},
    ]});
    let (output, request) = serve(&ws, "w1:p2", &["--mode", "play"], passed);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stdout(&output).ends_with("\nAll tests passed.\n"), "{}", stdout(&output));
    assert_eq!(request["name"], Value::Null);
    assert_eq!(request["mode"], "PlayMode");
    assert!(ws.calls_to(&["plugin", "pane", "open"]).is_empty());
}

#[test]
fn an_unknown_name_lists_the_suggestions() {
    let ws = Workspace::new(|dir| json!([shell("w1:p1", &dir.join("project"))]));
    let unknown = json!({"kind": "unknown_name", "suggestions": ["Game.BoardTests"]});
    let (output, _) = serve(&ws, "w1:p11", &["Wrong.BoardTests"], unknown);

    assert_eq!(output.status.code(), Some(1));
    let err = stderr(&output);
    assert!(err.contains("named exactly 'Wrong.BoardTests'"), "{err}");
    assert!(err.contains("Did you mean:\n  Game.BoardTests"), "{err}");
}

#[test]
fn compile_errors_are_printed_and_fail_the_run() {
    let ws = Workspace::new(|dir| json!([shell("w1:p1", &dir.join("project"))]));
    let errors = json!({"kind": "compile_failed", "errors": ["Board.cs(3,1): error CS1002"]});
    let (output, _) = serve(&ws, "w1:p11", &[], errors);

    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).contains("Compile errors (1)"), "{}", stdout(&output));
    assert!(stdout(&output).contains("  Board.cs(3,1): error CS1002"));
}

#[test]
fn a_pane_that_closes_mid_run_ends_the_wait() {
    let ws = Workspace::new(|dir| json!([shell("w1:p1", &dir.join("project"))]));
    let child = start(&ws, &[]);
    take(&mailbox(&ws, "w1:p11"));
    let state = json!({"panes": [shell("w1:p1", &ws.project())]});
    std::fs::write(ws.dir.join("state.json"), state.to_string()).unwrap();
    let output = child.wait_with_output().unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("pane w1:p11 closed before the run finished"));
}

#[test]
fn running_out_of_time_asks_the_pane_to_cancel() {
    let ws = Workspace::new(|dir| json!([shell("w1:p1", &dir.join("project"))]));
    let child = start(&ws, &["--timeout", "1"]);
    let dir = mailbox(&ws, "w1:p11");
    let (id, _) = take(&dir);
    let output = child.wait_with_output().unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("no outcome within 1s"), "{}", stderr(&output));
    assert!(dir.join(format!("{id}.withdrawn")).exists());
}

#[test]
fn running_outside_herdr_refuses() {
    let ws = Workspace::new(|dir| json!([shell("w1:p1", &dir.join("project"))]));
    let output = Command::new(env!("CARGO_BIN_EXE_herdr-runner"))
        .arg("run")
        .current_dir(ws.project())
        .env_remove("HERDR_WORKSPACE_ID")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("not inside herdr"));
}

#[test]
fn bad_arguments_print_the_usage() {
    let bin = env!("CARGO_BIN_EXE_herdr-runner");
    for args in [&["run", "--mode", "fast"][..], &["run", "A", "B"], &["run", "--bogus"]] {
        let output = Command::new(bin).args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(stderr(&output).contains("usage: herdr-runner run"), "{args:?}");
    }
}
