//! The toggle, open, and close actions, run as herdr runs them, against `tests/fakes/herdr.py`.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};

struct Workspace {
    dir: PathBuf,
}

impl Workspace {
    /// A Unity project with a nested script folder, and a workspace whose panes are `panes`.
    fn new(panes: impl FnOnce(&Path) -> Value) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "herdr-runner-actions-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("project/Assets/Scripts")).unwrap();
        std::fs::create_dir_all(dir.join("project/ProjectSettings")).unwrap();
        std::fs::create_dir_all(dir.join("elsewhere")).unwrap();
        let state = json!({"panes": panes(&dir)});
        std::fs::write(dir.join("state.json"), state.to_string()).unwrap();

        let fake = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fakes/herdr.py");
        let herdr = dir.join("herdr");
        std::fs::write(&herdr, format!("#!/bin/sh\nexec python3 '{}' \"$@\"\n", fake.display()))
            .unwrap();
        std::fs::set_permissions(&herdr, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self { dir }
    }

    fn project(&self) -> PathBuf {
        self.dir.join("project")
    }

    /// Run `--action name` with the env herdr gives an action invoked from pane `w1:p1`.
    fn act(&self, name: &str) -> Output {
        let context = json!({
            "workspace_id": "w1",
            "focused_pane_id": "w1:p1",
            "workspace_cwd": self.dir.join("elsewhere"),
        });
        Command::new(env!("CARGO_BIN_EXE_herdr-runner"))
            .args(["--action", name])
            .env("FAKE_HERDR_DIR", &self.dir)
            .env("HERDR_BIN_PATH", self.dir.join("herdr"))
            .env("HERDR_WORKSPACE_ID", "w1")
            .env("HERDR_PANE_ID", "w1:p1")
            .env("HERDR_PLUGIN_ID", "joachimmero.herdr-runner")
            .env("HERDR_PLUGIN_STATE_DIR", self.dir.join("plugin-state"))
            .env("HERDR_PLUGIN_CONTEXT_JSON", context.to_string())
            .output()
            .unwrap()
    }

    fn calls(&self) -> Vec<Vec<String>> {
        std::fs::read_to_string(self.dir.join("calls.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn calls_to(&self, prefix: &[&str]) -> Vec<Vec<String>> {
        self.calls()
            .into_iter()
            .filter(|call| {
                call.len() >= prefix.len() && call.iter().zip(prefix).all(|(a, b)| a == b)
            })
            .collect()
    }

    fn pane_ids(&self) -> Vec<String> {
        let state: Value =
            serde_json::from_str(&std::fs::read_to_string(self.dir.join("state.json")).unwrap())
                .unwrap();
        state["panes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pane| pane["pane_id"].as_str().unwrap().to_owned())
            .collect()
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn shell(id: &str, cwd: &Path) -> Value {
    json!({"pane_id": id, "foreground_cwd": cwd, "argv0": "-zsh", "argv": ["-zsh"]})
}

fn runner(id: &str, cwd: &Path, argv: &[&str]) -> Value {
    json!({"pane_id": id, "foreground_cwd": cwd, "argv0": "herdr-runner", "argv": argv})
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn option<'a>(call: &'a [String], name: &str) -> Option<&'a str> {
    call.iter().position(|arg| arg == name).map(|at| call[at + 1].as_str())
}

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
fn open_leaves_an_open_pane_alone() {
    let ws = Workspace::new(|dir| {
        json!([
            shell("w1:p1", &dir.join("project")),
            runner("w1:p2", &dir.join("project"), &["/p/bin/herdr-runner"])
        ])
    });
    let output = ws.act("open");
    assert!(output.status.success());
    assert_eq!(stdout(&output).trim(), "herdr-runner: already open (w1:p2)");
    assert!(ws.calls_to(&["plugin", "pane", "open"]).is_empty());
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
            runner("w1:p3", &project, &["herdr-runner"]),
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
