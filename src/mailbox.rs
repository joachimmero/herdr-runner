//! How `herdr-runner run` hands a run to a pane and gets its outcome back: a directory per pane
//! holding one file per stage of each request.
//!
//! A client writes `<id>.request`. The pane renames it to `<id>.taken` when it queues it, and
//! later writes `<id>.response`. A client that gives up writes `<id>.withdrawn`, and removes
//! every file of its request once it is done with it.

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::herdr;
use crate::unity::{Mode, TestOutcome};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub project: PathBuf,
    pub mode: Mode,
    /// A test's full name, a fixture's, or an assembly's; `None` runs everything.
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    Completed { results: Vec<TestOutcome> },
    CompileFailed { errors: Vec<String> },
    UnknownName { suggestions: Vec<String> },
    Cancelled,
    Failed { message: String },
}

pub struct Mailbox {
    dir: PathBuf,
}

impl Mailbox {
    /// Pane `pane`'s mailbox, under the plugin's state dir.
    pub fn for_pane(pane: &str) -> Result<Self, String> {
        Ok(Self::at(herdr::state_dir()?.join("mailbox").join(herdr::file_safe(pane))))
    }

    pub fn at(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Post `request` and return its id.
    pub fn send(&self, request: &Request) -> Result<String, String> {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
        let id = format!("{}-{nanos}", std::process::id());
        let json = serde_json::to_vec(request).map_err(|error| error.to_string())?;
        self.write(&id, "request", &json)?;
        Ok(id)
    }

    pub fn is_taken(&self, id: &str) -> bool {
        self.path(id, "taken").exists()
    }

    pub fn response(&self, id: &str) -> Option<Response> {
        let json = fs::read(self.path(id, "response")).ok()?;
        serde_json::from_slice(&json).ok()
    }

    pub fn withdraw(&self, id: &str) {
        let _ = self.write(id, "withdrawn", b"");
    }

    pub fn is_withdrawn(&self, id: &str) -> bool {
        self.path(id, "withdrawn").exists()
    }

    /// Remove every file of request `id`.
    pub fn clear(&self, id: &str) {
        for stage in ["request", "taken", "response", "withdrawn"] {
            let _ = fs::remove_file(self.path(id, stage));
        }
    }

    /// Mark every request not yet taken as taken, and return them oldest first. An unreadable
    /// request is answered at once, since its client would otherwise wait for nothing.
    pub fn take(&self) -> Vec<(String, Request)> {
        let Ok(entries) = fs::read_dir(&self.dir) else { return Vec::new() };
        let mut ids: Vec<String> = entries
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                entry.file_name().to_str()?.strip_suffix(".request").map(str::to_owned)
            })
            .collect();
        ids.sort_by_key(|id| sent_at(id));

        let mut taken = Vec::new();
        for id in ids {
            let (request, taken_path) = (self.path(&id, "request"), self.path(&id, "taken"));
            let parsed = fs::read(&request)
                .map_err(|error| error.to_string())
                .and_then(|json| serde_json::from_slice(&json).map_err(|error| error.to_string()));
            if fs::rename(&request, &taken_path).is_err() {
                continue;
            }
            match parsed {
                Ok(parsed) => taken.push((id, parsed)),
                Err(error) => {
                    let message = format!("the pane could not read the request: {error}");
                    self.respond(&id, &Response::Failed { message });
                }
            }
        }
        taken
    }

    pub fn respond(&self, id: &str, response: &Response) {
        if let Ok(json) = serde_json::to_vec(response) {
            let _ = self.write(id, "response", &json);
        }
    }

    /// Write through a temporary file, so a reader never sees half of one.
    fn write(&self, id: &str, stage: &str, contents: &[u8]) -> Result<(), String> {
        fs::create_dir_all(&self.dir)
            .map_err(|error| format!("cannot create {}: {error}", self.dir.display()))?;
        let partial = self.path(id, &format!("{stage}.partial"));
        fs::write(&partial, contents)
            .and_then(|()| fs::rename(&partial, self.path(id, stage)))
            .map_err(|error| format!("cannot write to {}: {error}", self.dir.display()))
    }

    fn path(&self, id: &str, stage: &str) -> PathBuf {
        self.dir.join(format!("{id}.{stage}"))
    }
}

/// The send time an id carries after its pid.
fn sent_at(id: &str) -> u128 {
    id.rsplit('-').next().and_then(|nanos| nanos.parse().ok()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mailbox(name: &str) -> Mailbox {
        let dir = std::env::temp_dir()
            .join(format!("herdr-runner-mailbox-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        Mailbox::at(dir)
    }

    fn request(name: &str) -> Request {
        Request { project: "/p".into(), mode: Mode::EditMode, name: Some(name.into()) }
    }

    #[test]
    fn a_request_is_taken_once_and_answered() {
        let mailbox = mailbox("roundtrip");
        let id = mailbox.send(&request("A.B")).unwrap();
        assert!(!mailbox.is_taken(&id));

        let taken = mailbox.take();
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].0, id);
        assert_eq!(taken[0].1.name.as_deref(), Some("A.B"));
        assert!(mailbox.is_taken(&id));
        assert!(mailbox.take().is_empty());

        assert!(mailbox.response(&id).is_none());
        mailbox.respond(&id, &Response::Cancelled);
        assert!(matches!(mailbox.response(&id), Some(Response::Cancelled)));
        mailbox.clear(&id);
        assert_eq!(fs::read_dir(&mailbox.dir).unwrap().count(), 0);
    }

    #[test]
    fn requests_are_taken_oldest_first() {
        let mailbox = mailbox("order");
        let first = mailbox.send(&request("First")).unwrap();
        let second = mailbox.send(&request("Second")).unwrap();
        let ids: Vec<String> = mailbox.take().into_iter().map(|(id, _)| id).collect();
        assert_eq!(ids, [first, second]);
    }

    #[test]
    fn an_unreadable_request_is_answered_with_a_failure() {
        let mailbox = mailbox("garbled");
        fs::create_dir_all(&mailbox.dir).unwrap();
        fs::write(mailbox.dir.join("1-1.request"), "not json").unwrap();
        assert!(mailbox.take().is_empty());
        assert!(matches!(mailbox.response("1-1"), Some(Response::Failed { .. })));
    }

    #[test]
    fn a_withdrawn_request_says_so() {
        let mailbox = mailbox("withdrawn");
        let id = mailbox.send(&request("A.B")).unwrap();
        assert!(!mailbox.is_withdrawn(&id));
        mailbox.withdraw(&id);
        assert!(mailbox.is_withdrawn(&id));
    }
}
