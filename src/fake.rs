//! A scripted Editor behind `tests/fakes/unity.py`, one per test so tests run in parallel.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::{Value, json};

use crate::unity::{Editor, Pace};

pub struct FakeEditor {
    dir: PathBuf,
    cli: PathBuf,
}

/// Fast enough that a test polling a few times stays well under a second.
pub const PACE: Pace =
    Pace { poll: Duration::from_millis(10), unreachable: Duration::from_millis(300) };

impl FakeEditor {
    /// A project directory and an Editor scripted by `scenario` (see the fake's docstring), whose
    /// `project` defaults to that directory.
    pub fn new(mut scenario: Value) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "herdr-runner-fake-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let project = dir.join("project");
        std::fs::create_dir_all(project.join("Assets")).unwrap();
        std::fs::create_dir_all(project.join("ProjectSettings")).unwrap();
        if scenario.get("project").is_none() {
            scenario["project"] = json!(project);
        }
        std::fs::write(dir.join("scenario.json"), scenario.to_string()).unwrap();

        let fake = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fakes/unity.py");
        let cli = write_python_shim(&dir.join("unity"), &fake, &[dir.as_path()]);
        Self { dir, cli }
    }

    pub fn project(&self) -> PathBuf {
        self.dir.join("project")
    }

    pub fn editor(&self) -> Editor {
        Editor::with_cli(self.project(), self.cli.clone(), PACE)
    }

    /// Every call the Editor received, oldest first.
    pub fn calls(&self) -> Vec<Value> {
        std::fs::read_to_string(self.dir.join("calls.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    pub fn commands(&self) -> Vec<String> {
        self.calls().iter().map(|call| call["command"].as_str().unwrap().to_owned()).collect()
    }

    /// The arguments of each `run_tests` call.
    pub fn runs(&self) -> Vec<Value> {
        self.calls()
            .into_iter()
            .filter(|call| call["command"] == "run_tests")
            .map(|call| call["args"].clone())
            .collect()
    }
}

impl Drop for FakeEditor {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn python() -> &'static Path {
    static PYTHON: OnceLock<PathBuf> = OnceLock::new();
    PYTHON.get_or_init(find_python)
}

fn find_python() -> PathBuf {
    for candidate in python_candidates() {
        let output =
            Command::new(&candidate).args(["-c", "import sys; print(sys.executable)"]).output();
        if let Ok(output) = output
            && output.status.success()
        {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if !path.is_empty() {
                return PathBuf::from(path);
            }
        }
    }
    panic!("python3 or python is required to run the tests (see README)");
}

fn python_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        if let Ok(entries) = std::fs::read_dir(PathBuf::from(local).join("Programs").join("Python"))
        {
            for entry in entries.flatten() {
                let python = entry.path().join("python.exe");
                if python.is_file() {
                    candidates.push(python);
                }
            }
        }
    }
    candidates.push(PathBuf::from("python3"));
    candidates.push(PathBuf::from("python"));
    candidates
}

fn write_python_shim(dest: &Path, script: &Path, extras: &[&Path]) -> PathBuf {
    #[cfg(windows)]
    let dest = dest.with_extension("cmd");
    #[cfg(unix)]
    let dest = dest.to_path_buf();
    let mut command = format!("\"{}\" \"{}\"", python().display(), script.display());
    for extra in extras {
        command.push_str(&format!(" \"{}\"", extra.display()));
    }
    #[cfg(windows)]
    let body = format!("@echo off\r\n{command} %*\r\n");
    #[cfg(unix)]
    let body = format!("#!/bin/sh\nexec {command} \"$@\"\n");
    std::fs::write(&dest, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    dest
}

/// A small suite: two fixtures, a failure with a stack trace, an explicit test, and a fixture
/// whose name extends another's.
pub fn suite() -> Value {
    json!({
        "editor": [
            {"FullName": "Game.BoardTests.PlacesTile", "Assembly": "Game.Tests"},
            {"FullName": "Game.BoardTests.RejectsOverlap", "Assembly": "Game.Tests",
             "outcome": {"Status": "Failed", "Message": "Expected: 1\n  But was: 2",
                         "StackTrace": "at Game.BoardTests.RejectsOverlap () [0x00001] in BoardTests.cs:42"}},
            {"FullName": "Game.BoardTestsExtra.Scores(1.5)", "Assembly": "Game.Tests"},
            {"FullName": "Game.Sim.RunsForever", "Assembly": "Game.Tests", "Explicit": true},
            {"FullName": "Words.DictionaryTests.Loads", "Assembly": "Words.Tests"},
        ],
        "playmode": [
            {"FullName": "Game.PlayTests.Boots", "Assembly": "Game.PlayTests"},
        ],
    })
}
