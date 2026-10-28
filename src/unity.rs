//! The open Editor, driven through the `unity` CLI's `command` verb.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Which of the Test Runner's two suites a call addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    EditMode,
    PlayMode,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Self::EditMode => "EditMode",
            Self::PlayMode => "PlayMode",
        }
    }

    pub fn other(self) -> Self {
        match self {
            Self::EditMode => Self::PlayMode,
            Self::PlayMode => Self::EditMode,
        }
    }

    fn arg(self) -> &'static str {
        match self {
            Self::EditMode => "editor",
            Self::PlayMode => "playmode",
        }
    }
}

/// One test `list_tests` reports. The Pipeline package has emitted both casings over time.
#[derive(Debug, Clone, Deserialize)]
pub struct TestItem {
    #[serde(rename = "fullName", alias = "FullName")]
    pub full_name: String,
    #[serde(default, alias = "Assembly")]
    pub assembly: Option<String>,
    #[serde(default, alias = "Explicit")]
    pub explicit: bool,
}

/// A finished test's outcome, as the async run's status file reports it.
#[derive(Debug, Clone, Deserialize)]
pub struct TestOutcome {
    #[serde(rename = "FullName", alias = "fullName")]
    pub full_name: String,
    #[serde(rename = "Status", alias = "status", deserialize_with = "verdict")]
    pub verdict: Verdict,
    #[serde(rename = "Duration", alias = "duration", default)]
    pub duration: f64,
    #[serde(rename = "Message", alias = "message", default)]
    pub message: Option<String>,
    #[serde(rename = "StackTrace", alias = "stackTrace", default)]
    pub stack_trace: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Passed,
    Failed,
    Skipped,
    Inconclusive,
}

fn verdict<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Verdict, D::Error> {
    let status = String::deserialize(deserializer)?;
    Ok(match status.to_ascii_lowercase().as_str() {
        "passed" => Verdict::Passed,
        "failed" => Verdict::Failed,
        "skipped" => Verdict::Skipped,
        _ => Verdict::Inconclusive,
    })
}

/// What `run_tests` matches its filter against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterType {
    TestName,
    Assembly,
}

/// One `run_tests` call's selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRequest {
    pub filter: String,
    pub filter_type: FilterType,
    pub include_explicit: bool,
}

impl RunRequest {
    pub fn all() -> Self {
        Self { filter: String::new(), filter_type: FilterType::TestName, include_explicit: false }
    }

    pub fn assembly(name: &str) -> Self {
        Self { filter: name.to_owned(), filter_type: FilterType::Assembly, include_explicit: false }
    }

    /// The trailing dot keeps `Fixture` from also matching `FixtureTwo`.
    pub fn fixture(name: &str) -> Self {
        Self {
            filter: format!("{name}."),
            filter_type: FilterType::TestName,
            include_explicit: false,
        }
    }

    /// A test marked `[Explicit]` only runs when asked for, so asking for it by name includes it.
    pub fn test(full_name: &str, explicit: bool) -> Self {
        Self {
            filter: full_name.to_owned(),
            filter_type: FilterType::TestName,
            include_explicit: explicit,
        }
    }

    /// Whether Unity will run this test for the request: a case-insensitive substring match,
    /// which can also sweep in a test whose name merely contains the one asked for.
    pub fn covers(&self, assembly: &str, full_name: &str, explicit: bool) -> bool {
        if explicit && !self.include_explicit {
            return false;
        }
        let haystack = match self.filter_type {
            FilterType::TestName => full_name,
            FilterType::Assembly => assembly,
        };
        haystack.to_lowercase().contains(&self.filter.to_lowercase())
    }
}

/// How a recompile ended.
#[derive(Debug)]
pub enum Compile {
    Clean,
    /// Unity's error lines; empty when the Editor reports errors without listing them.
    Failed(Vec<String>),
}

/// What `test_status` reports about the last async run.
#[derive(Debug)]
pub enum RunStatus {
    Running,
    Completed(Vec<TestOutcome>),
    Cancelled,
    Failed(String),
    /// Neither a run in flight nor one on record.
    Idle,
}

/// The Editor that has one project open, reached through the CLI at `cli`.
#[derive(Clone)]
pub struct Editor {
    project: PathBuf,
    cli: PathBuf,
    pace: Pace,
}

/// How often to poll the Editor, and how long it may stay unreachable mid-poll, as it does
/// across a domain reload.
#[derive(Debug, Clone, Copy)]
pub struct Pace {
    pub poll: Duration,
    pub unreachable: Duration,
}

impl Pace {
    pub const EDITOR: Self =
        Self { poll: Duration::from_millis(500), unreachable: Duration::from_secs(60) };
}

impl Editor {
    pub fn new(project: PathBuf) -> Self {
        Self::with_cli(project, unity_bin(), Pace::EDITOR)
    }

    pub fn with_cli(project: PathBuf, cli: PathBuf, pace: Pace) -> Self {
        Self { project, cli, pace }
    }

    pub fn pace(&self) -> Pace {
        self.pace
    }

    /// Refuse an Editor that is unreachable or has another project open: `--project-path` reaches
    /// whichever Editor holds the project, which in a worktree setup can be the primary checkout.
    pub fn check(&self) -> Result<(), String> {
        #[derive(Deserialize)]
        struct Status {
            #[serde(rename = "projectPath", default)]
            project_path: String,
        }
        let status: Status = self.call_as("editor_status", &[], 30)?;
        if status.project_path.is_empty()
            || same_dir(Path::new(&status.project_path), &self.project)
        {
            return Ok(());
        }
        Err(format!(
            "the reachable Editor has {} open, not {}",
            status.project_path,
            self.project.display()
        ))
    }

    pub fn list_tests(&self, mode: Mode) -> Result<Vec<TestItem>, String> {
        #[derive(Deserialize)]
        struct Listing {
            #[serde(default, alias = "Success")]
            success: Option<bool>,
            #[serde(default, alias = "Error")]
            error: Option<String>,
            #[serde(default, alias = "Tests")]
            tests: Vec<TestItem>,
        }
        let listing: Listing = self.call_as("list_tests", &[("mode", mode.arg())], 120)?;
        match listing.success {
            Some(false) => Err(listing.error.unwrap_or_else(|| "list_tests failed".into())),
            _ => Ok(listing.tests),
        }
    }

    /// Import, compile, and wait out the domain reload, so a run that follows tests fresh code.
    pub fn recompile(&self) -> Result<Compile, String> {
        let started = self.call("recompile", &[], 60)?;
        match text(&started, "status").as_deref() {
            Some("up_to_date") => return Ok(Compile::Clean),
            Some("failed") => return Ok(Compile::Failed(self.compile_errors())),
            _ => {}
        }

        let deadline = Instant::now() + Duration::from_secs(300);
        let mut contact = Instant::now();
        loop {
            if Instant::now() >= deadline {
                return Err("the recompile did not finish within 300s".into());
            }
            thread::sleep(self.pace.poll);
            let Ok(status) = self.call("recompile_status", &[], 30) else {
                if contact.elapsed() > self.pace.unreachable {
                    return Err("lost contact with the Editor while compiling".into());
                }
                continue;
            };
            contact = Instant::now();
            match text(&status, "status").as_deref() {
                Some("completed") if status["failed"] == Value::Bool(true) => {
                    return Ok(Compile::Failed(strings(&status["errors"])));
                }
                Some("completed" | "up_to_date") => break,
                _ => {}
            }
        }
        self.settle()?;
        Ok(Compile::Clean)
    }

    fn compile_errors(&self) -> Vec<String> {
        self.call("recompile_status", &[], 30)
            .map(|status| strings(&status["errors"]))
            .unwrap_or_default()
    }

    /// Wait until the Editor is neither compiling nor reloading its domain.
    fn settle(&self) -> Result<(), String> {
        let deadline = Instant::now() + self.pace.unreachable;
        while Instant::now() < deadline {
            if let Ok(status) = self.call("editor_status", &[], 30)
                && status["compiling"] != Value::Bool(true)
                && status["domainReloadInProgress"] != Value::Bool(true)
            {
                return Ok(());
            }
            thread::sleep(self.pace.poll);
        }
        Err("the Editor was still reloading after the recompile".into())
    }

    /// Start an async run, whose progress [`Editor::run_status`] reports.
    pub fn start_run(&self, mode: Mode, request: &RunRequest) -> Result<(), String> {
        let filter_type = match request.filter_type {
            FilterType::TestName => "testName",
            FilterType::Assembly => "assembly",
        };
        let include_explicit = if request.include_explicit { "true" } else { "false" };
        let mut args = vec![
            ("mode", mode.arg()),
            ("filter_type", filter_type),
            ("include_explicit", include_explicit),
            ("async_tests", "true"),
        ];
        if !request.filter.is_empty() {
            args.push(("filter", request.filter.as_str()));
        }
        let started = self.call("run_tests", &args, 60)?;
        let success = started.get("success").or_else(|| started.get("Success"));
        if success == Some(&Value::Bool(false)) {
            return Err(text(&started, "error")
                .or_else(|| text(&started, "Error"))
                .unwrap_or_else(|| "run_tests refused the run".into()));
        }
        Ok(())
    }

    pub fn run_status(&self) -> Result<RunStatus, String> {
        let status = self.call("test_status", &[], 30)?;
        Ok(match text(&status, "status").as_deref() {
            Some("running") => RunStatus::Running,
            Some("completed") => RunStatus::Completed(
                serde_json::from_value(status["results"].clone())
                    .map_err(|error| format!("unreadable test results: {error}"))?,
            ),
            Some("cancelled") => RunStatus::Cancelled,
            Some("error") => RunStatus::Failed(
                text(&status, "message").unwrap_or_else(|| "the test run failed".into()),
            ),
            _ => RunStatus::Idle,
        })
    }

    pub fn cancel_run(&self) -> Result<(), String> {
        self.call("cancel_tests", &[], 30).map(drop)
    }

    fn call_as<T: DeserializeOwned>(
        &self,
        command: &str,
        args: &[(&str, &str)],
        timeout_secs: u32,
    ) -> Result<T, String> {
        serde_json::from_value(self.call(command, args, timeout_secs)?)
            .map_err(|error| format!("{command} answered in an unknown shape: {error}"))
    }

    /// The command's `data.result`, which the CLI sometimes hands over as a JSON-encoded string.
    fn call(
        &self,
        command: &str,
        args: &[(&str, &str)],
        timeout_secs: u32,
    ) -> Result<Value, String> {
        let output = Command::new(&self.cli)
            .env("PATH", search_path())
            .arg("command")
            .arg("--project-path")
            .arg(&self.project)
            .args(["--timeout", &timeout_secs.to_string(), command])
            .args(args.iter().flat_map(|(key, value)| [format!("--{key}"), (*value).to_owned()]))
            .args(["--json", "--no-banner", "--non-interactive"])
            .stdin(Stdio::null())
            .output()
            .map_err(|error| format!("could not run the unity CLI: {error}"))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let envelope: Value = serde_json::from_str(stdout.trim())
            .or_else(|_| serde_json::from_str(stderr.trim()))
            .map_err(|_| {
                first_line(&stdout).or_else(|| first_line(&stderr)).unwrap_or_else(|| {
                    format!("unity command {command} exited with {}", output.status)
                })
            })?;
        if envelope["success"] != Value::Bool(true) {
            return Err(describe_errors(&envelope["errors"])
                .unwrap_or_else(|| format!("unity command {command} failed")));
        }
        let result = envelope.pointer("/data/result").cloned().unwrap_or(Value::Null);
        Ok(match result {
            Value::String(encoded) => {
                serde_json::from_str(&encoded).unwrap_or(Value::String(encoded))
            }
            other => other,
        })
    }
}

/// The CLI installs to `~/.unity/bin`, which a pane's non-login shell may not have on `PATH`.
fn unity_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| Path::new(&home).join(".unity/bin"))
}

fn unity_bin() -> PathBuf {
    unity_dir()
        .map(|dir| dir.join("unity"))
        .filter(|bin| bin.is_file())
        .unwrap_or_else(|| PathBuf::from("unity"))
}

fn search_path() -> OsString {
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let dirs = unity_dir().into_iter().chain(std::env::split_paths(&inherited));
    std::env::join_paths(dirs).unwrap_or(inherited)
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|item| item.as_str().map_or_else(|| item.to_string(), str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// The CLI's `errors` as one line: each entry is a string or an object carrying a `message`.
fn describe_errors(errors: &Value) -> Option<String> {
    let lines: Vec<String> = errors
        .as_array()?
        .iter()
        .map(|error| match error {
            Value::String(message) => message.clone(),
            other => text(other, "message").unwrap_or_else(|| other.to_string()),
        })
        .collect();
    (!lines.is_empty()).then(|| lines.join("; "))
}

fn first_line(text: &str) -> Option<String> {
    text.lines().map(str::trim).find(|line| !line.is_empty()).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fixture_request_skips_a_fixture_whose_name_extends_it() {
        let request = RunRequest::fixture("Game.BoardTests");
        assert!(request.covers("Tests", "Game.BoardTests.PlacesTile", false));
        assert!(!request.covers("Tests", "Game.BoardTestsExtra.PlacesTile", false));
    }

    #[test]
    fn explicit_tests_run_only_when_asked_for_by_name() {
        let name = "Game.Sim.RunsForever";
        assert!(!RunRequest::all().covers("Tests", name, true));
        assert!(!RunRequest::fixture("Game.Sim").covers("Tests", name, true));
        assert!(RunRequest::test(name, true).covers("Tests", name, true));
    }

    #[test]
    fn an_assembly_request_matches_on_the_assembly() {
        let request = RunRequest::assembly("Flywheel.Tests.Editor");
        assert!(request.covers("Flywheel.Tests.Editor", "Anything.At.All", false));
        assert!(!request.covers("Other.Tests", "Flywheel.Tests.Editor.Named", false));
    }

    #[test]
    fn outcomes_read_in_either_casing() {
        let pascal = r#"{"FullName":"A.B","Status":"Failed","Duration":0.5,"Message":"boom"}"#;
        let camel = r#"{"fullName":"A.B","status":"Passed"}"#;
        let pascal: TestOutcome = serde_json::from_str(pascal).unwrap();
        let camel: TestOutcome = serde_json::from_str(camel).unwrap();
        assert_eq!(pascal.verdict, Verdict::Failed);
        assert_eq!(pascal.message.as_deref(), Some("boom"));
        assert_eq!(camel.verdict, Verdict::Passed);
    }

    #[test]
    fn listed_tests_read_in_either_casing() {
        let item: TestItem =
            serde_json::from_str(r#"{"FullName":"A.B","Assembly":"Tests","Explicit":true}"#)
                .unwrap();
        assert_eq!(item.assembly.as_deref(), Some("Tests"));
        assert!(item.explicit);
    }
}
