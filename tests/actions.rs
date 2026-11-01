//! The toggle, open, and close actions, run as herdr runs them, against `tests/fakes/herdr.py`.

mod common;

use std::process::Command;

use common::{Workspace, option, runner, shell, stderr, stdout};
use serde_json::json;

#[test]
fn toggle_opens_a_split_beside_the_focused_pane_for_its_project() {
    let ws = Workspace::new(|dir| json!([shell("w1:p1", &dir.join("project/Assets/Scripts"))]));
    let output = ws.act("toggle");
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).starts_with("herdr-runner: opened w1:p11"), "{}", stdout(&output));

    let opens = ws.calls_to(&["plugin", "pane", "open"]);
    assert_eq!(opens.len(), 1);
    let open = &opens[0];
    assert_eq!(option(open, "--plugin"), Some("joachimmero.herdr-runner"));
    assert_eq!(option(open, "--entrypoint"), Some("pane"));
    assert_eq!(option(open, "--placement"), Some("split"));
    assert_eq!(option(open, "--target-pane"), Some("w1:p1"));
    assert_eq!(option(open, "--direction"), Some("right"));
    // The project root, not the nested folder the shell stood in.
    assert_eq!(option(open, "--cwd"), Some(ws.project().to_str().unwrap()));
    assert!(open.contains(&"--focus".to_owned()));
}

#[test]
fn toggle_again_closes_the_pane_it_opened() {
    let ws = Workspace::new(|dir| json!([shell("w1:p1", &dir.join("project"))]));
    assert!(ws.act("toggle").status.success());
    let output = ws.act("toggle");
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output).trim(), "herdr-runner: closed w1:p11");
    assert_eq!(ws.pane_ids(), ["w1:p1"]);
}

#[test]
fn open_focuses_an_open_pane() {
    let ws = Workspace::new(|dir| {
        json!([
            shell("w1:p1", &dir.join("project")),
            runner("w1:p2", &dir.join("project"), &["/p/bin/herdr-runner"])
        ])
    });
    let output = ws.act("open");
    assert!(output.status.success());
    assert_eq!(stdout(&output).trim(), "herdr-runner: focused w1:p2");
    assert!(ws.calls_to(&["plugin", "pane", "open"]).is_empty());
    assert_eq!(ws.calls_to(&["plugin", "pane", "focus"]), [["plugin", "pane", "focus", "w1:p2"]]);
}

#[test]
fn close_with_nothing_open_does_nothing() {
    let ws = Workspace::new(|dir| json!([shell("w1:p1", &dir.join("project"))]));
    let output = ws.act("close");
    assert!(output.status.success());
    assert_eq!(stdout(&output).trim(), "herdr-runner: nothing open");
    assert!(ws.calls_to(&["pane", "close"]).is_empty());
}

#[test]
fn close_closes_every_runner_pane_in_the_workspace() {
    let ws = Workspace::new(|dir| {
        let project = dir.join("project");
        json!([
            shell("w1:p1", &project),
            runner("w1:p2", &project, &["/p/bin/herdr-runner"]),
            runner("w1:p3", &project, &[r"C:\bin\herdr-runner.EXE"]),
        ])
    });
    let output = ws.act("close");
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(ws.pane_ids(), ["w1:p1"]);
}

#[test]
fn a_pane_running_an_action_is_not_the_ui() {
    let ws = Workspace::new(|dir| {
        let project = dir.join("project");
        json!([
            shell("w1:p1", &project),
            runner("w1:p2", &project, &["bin/herdr-runner", "--action", "toggle"])
        ])
    });
    assert!(ws.act("toggle").status.success());
    assert_eq!(ws.calls_to(&["plugin", "pane", "open"]).len(), 1);
    assert!(ws.calls_to(&["pane", "close"]).is_empty());
}

#[test]
fn opening_outside_a_unity_project_refuses() {
    let ws = Workspace::new(|dir| json!([shell("w1:p1", &dir.join("elsewhere"))]));
    let output = ws.act("toggle");
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("no Unity project at or above"), "{}", stderr(&output));
    assert!(ws.calls_to(&["plugin", "pane", "open"]).is_empty());
}

#[test]
fn an_action_outside_herdr_refuses() {
    let output = Command::new(env!("CARGO_BIN_EXE_herdr-runner"))
        .args(["--action", "toggle"])
        .env_remove("HERDR_WORKSPACE_ID")
        .env_remove("HERDR_PLUGIN_CONTEXT_JSON")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("no workspace context"));
}

#[test]
fn unknown_actions_and_arguments_are_refused() {
    let bin = env!("CARGO_BIN_EXE_herdr-runner");
    let unknown = Command::new(bin).args(["--action", "explode"]).output().unwrap();
    assert_eq!(unknown.status.code(), Some(1));
    assert!(stderr(&unknown).contains("unknown action 'explode'"));
    let usage = Command::new(bin).arg("--bogus").output().unwrap();
    assert_eq!(usage.status.code(), Some(2));
}
