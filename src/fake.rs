//! A scripted Editor behind `tests/fakes/unity.py`, one per test so tests run in parallel.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::{Value, json};

use crate::unity::{Editor, Pace};

pub struct FakeEditor {
    dir: PathBuf,
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
        let cli = dir.join("unity");
        let script =
            format!("#!/bin/sh\nexec python3 '{}' '{}' \"$@\"\n", fake.display(), dir.display());
        std::fs::write(&cli, script).unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self { dir }
    }

    pub fn project(&self) -> PathBuf {
        self.dir.join("project")
    }

    pub fn editor(&self) -> Editor {
        Editor::with_cli(self.project(), self.dir.join("unity"), PACE)
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
