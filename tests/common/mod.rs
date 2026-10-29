//! A scripted herdr workspace around a Unity project, shared by the binary's integration tests.

#![allow(dead_code)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};

pub struct Workspace {
    pub dir: PathBuf,
}

impl Workspace {
    /// A Unity project with a nested script folder, and a workspace whose panes are `panes`.
    pub fn new(panes: impl FnOnce(&Path) -> Value) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "herdr-runner-workspace-{}-{}",
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

    pub fn project(&self) -> PathBuf {
        self.dir.join("project")
    }

    /// Run `--action name` with the env herdr gives an action invoked from pane `w1:p1`.
    pub fn act(&self, name: &str) -> Output {
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

    pub fn calls(&self) -> Vec<Vec<String>> {
        std::fs::read_to_string(self.dir.join("calls.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    pub fn calls_to(&self, prefix: &[&str]) -> Vec<Vec<String>> {
        self.calls()
            .into_iter()
            .filter(|call| {
                call.len() >= prefix.len() && call.iter().zip(prefix).all(|(a, b)| a == b)
            })
            .collect()
    }

    pub fn pane_ids(&self) -> Vec<String> {
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

pub fn shell(id: &str, cwd: &Path) -> Value {
    json!({"pane_id": id, "foreground_cwd": cwd, "argv0": "-zsh", "argv": ["-zsh"]})
}

pub fn runner(id: &str, cwd: &Path, argv: &[&str]) -> Value {
    json!({"pane_id": id, "foreground_cwd": cwd, "argv0": "herdr-runner", "argv": argv})
}

pub fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

pub fn option<'a>(call: &'a [String], name: &str) -> Option<&'a str> {
    call.iter().position(|arg| arg == name).map(|at| call[at + 1].as_str())
}
